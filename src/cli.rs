use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};

use crate::transport::server::HEALTH_PATH;

pub const TOKEN_ENV: &str = "MCPD_TOKEN";

const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_NAME: &str = "mcpd";
const DEFAULT_MCP_PATH: &str = "/mcp";
const DEFAULT_LIST_TTL_MS: u64 = 60_000;

#[derive(Parser, Serialize, Deserialize, Debug, Default)]
#[command(
    name = "mcpd",
    version,
    about = "Serve MCP over HTTP, or over stdio with --stdio. Every tool is a command on this machine."
)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Cli {
    #[arg(
        short = 'c',
        long,
        value_name = "FILE",
        help = "TOML or JSON file holding any of these settings. Flags win over it"
    )]
    #[serde(skip)]
    pub config: Option<PathBuf>,

    #[arg(
        long,
        help = "Serve without authentication, trusting every caller. Must be typed at launch"
    )]
    #[serde(skip_deserializing)]
    pub no_auth: bool,

    #[arg(
        long,
        help = "Serve one client on stdin and stdout instead of over HTTP. Nothing is bound, and nothing is authenticated"
    )]
    #[serde(default)]
    pub stdio: bool,

    #[arg(
        long,
        value_name = "ADDR",
        help = "Listen address [default: 127.0.0.1:8080]"
    )]
    pub bind: Option<String>,

    #[arg(
        long,
        value_name = "NAME",
        help = "Server name reported in serverInfo [default: mcpd]"
    )]
    pub name: Option<String>,

    #[arg(
        long,
        value_name = "TITLE",
        help = "Display name reported in serverInfo. Defaults to none, leaving clients the name"
    )]
    pub title: Option<String>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Path the MCP endpoint is served at [default: /mcp]"
    )]
    pub mcp_path: Option<String>,

    #[arg(
        long,
        value_name = "DIR",
        help = "Working directory for tools that declare none. Defaults to the daemon's own"
    )]
    pub cwd: Option<PathBuf>,

    #[arg(
        long = "tool",
        value_name = "JSON",
        help = "One tool definition, as JSON. Repeatable"
    )]
    #[serde(default, rename = "tool", deserialize_with = "definitions")]
    pub tools: Vec<String>,

    #[arg(
        long,
        value_name = "TEXT",
        help = "Server instructions returned at initialize and server/discover"
    )]
    pub instructions: Option<String>,

    #[arg(
        long,
        value_name = "MS",
        help = "How long clients may cache tools/list. 0 revalidates every call [default: 60000]"
    )]
    pub list_ttl_ms: Option<u64>,
}

/// What the two sources agreed on, with defaults filled in. Deserialized from
/// the merged settings rather than assembled field by field, so a field added
/// to `Cli` cannot be left out of the merge.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    no_auth: bool,
    #[serde(default)]
    stdio: bool,
    #[serde(default = "default_bind")]
    bind: String,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default = "default_mcp_path")]
    mcp_path: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default, rename = "tool")]
    pub tools: Vec<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default = "default_list_ttl_ms")]
    pub list_ttl_ms: u64,
}

/// The fields that only mean something over HTTP, folded into the transport
/// they belong to. `coherent` refuses the contradictory flags; this shape is
/// why nothing downstream has to remember which fields those were.
pub enum Transport {
    Stdio,
    Http(HttpEndpoint),
}

pub struct HttpEndpoint {
    pub bind: String,
    pub mcp_path: String,
    pub no_auth: bool,
}

impl Settings {
    pub fn transport(&self) -> Transport {
        if self.stdio {
            Transport::Stdio
        } else {
            Transport::Http(HttpEndpoint {
                bind: self.bind.clone(),
                mcp_path: self.mcp_path.clone(),
                no_auth: self.no_auth,
            })
        }
    }
}

fn default_bind() -> String {
    DEFAULT_BIND.to_string()
}

fn default_name() -> String {
    DEFAULT_NAME.to_string()
}

fn default_mcp_path() -> String {
    DEFAULT_MCP_PATH.to_string()
}

fn default_list_ttl_ms() -> u64 {
    DEFAULT_LIST_TTL_MS
}

impl Cli {
    pub fn load(self) -> anyhow::Result<Settings> {
        self.coherent()?;

        let file = match self.config.as_deref() {
            Some(path) => read_config(path)?,
            None => Cli::default(),
        };

        self.over(file)
    }

    fn coherent(&self) -> anyhow::Result<()> {
        if !self.stdio {
            return Ok(());
        }

        for (flag, given) in [
            ("--bind", self.bind.is_some()),
            ("--mcp-path", self.mcp_path.is_some()),
            ("--no-auth", self.no_auth),
        ] {
            if given {
                anyhow::bail!(
                    "{flag} has no meaning with --stdio: nothing is served over HTTP, and the \
                     client on the other end of the pipe is whoever launched this process"
                );
            }
        }

        Ok(())
    }

    fn over(self, file: Cli) -> anyhow::Result<Settings> {
        let mut merged = Map::new();
        for source in [&file, &self] {
            for (key, value) in keys(source)? {
                if !unset(&value) {
                    merged.insert(key, value);
                }
            }
        }

        serde_json::from_value(Value::Object(merged)).context("cannot resolve these settings")
    }
}

fn keys(cli: &Cli) -> anyhow::Result<Map<String, Value>> {
    match serde_json::to_value(cli)? {
        Value::Object(keys) => Ok(keys),
        other => anyhow::bail!("settings are not a table: {other}"),
    }
}

/// What a flag that was never typed looks like once serialized. A flag is
/// absent or it is a value; there is no `--bind ""` that means "unset", and no
/// way to spell a `false` that should overrule a file.
fn unset(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => true,
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Decided before the file is read, so an extension nothing can parse is
/// reported as such whether or not the path exists.
fn read_config(path: &Path) -> anyhow::Result<Cli> {
    enum Format {
        Toml,
        Json,
    }

    let format = match path.extension().and_then(|e| e.to_str()) {
        Some("toml") => Format::Toml,
        Some("json") => Format::Json,
        _ => anyhow::bail!("config file {} must end in .toml or .json", path.display()),
    };

    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read config file {}", path.display()))?;

    let parsed = match format {
        Format::Toml => toml::from_str(&text).map_err(anyhow::Error::from),
        Format::Json => serde_json::from_str(&text).map_err(anyhow::Error::from),
    };

    parsed.with_context(|| format!("cannot parse config file {}", path.display()))
}

/// A tool is written either the way the flag takes it — one JSON string — or as
/// a native table. Both end up as the JSON the loader already parses.
fn definitions<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Definition {
        Json(String),
        Mapping(serde_json::Value),
    }

    Vec::<Definition>::deserialize(deserializer)?
        .into_iter()
        .map(|definition| match definition {
            Definition::Json(json) => Ok(json),
            Definition::Mapping(value) => serde_json::to_string(&value).map_err(de::Error::custom),
        })
        .collect()
}

/// The bearer token, or `None` when the caller has said in as many words that
/// there should not be one. Every combination is decided here so that turning
/// authentication off is never something that merely happens.
pub fn credentials(no_auth: bool) -> anyhow::Result<Option<String>> {
    let present = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
    decide(no_auth, present)
}

fn decide(no_auth: bool, present: Option<String>) -> anyhow::Result<Option<String>> {
    match (no_auth, present) {
        (true, Some(_)) => anyhow::bail!(
            "--no-auth was passed but {TOKEN_ENV} is also set; unset it or drop the flag"
        ),
        (true, None) => Ok(None),
        (false, Some(token)) => Ok(Some(token)),
        (false, None) => anyhow::bail!(
            "{TOKEN_ENV} must be set to a non-empty bearer token, or pass --no-auth to serve without one"
        ),
    }
}

/// axum panics on a route that does not start with `/`, and on a second route
/// registered at a path it already serves. Both become startup errors here.
pub fn mcp_path(configured: String) -> anyhow::Result<String> {
    if !configured.starts_with('/') {
        anyhow::bail!("MCP path `{configured}` must start with `/`");
    }
    if configured == HEALTH_PATH {
        anyhow::bail!("MCP path `{configured}` is already taken by the health check");
    }
    Ok(configured)
}

pub fn working_dir(configured: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let requested = match configured {
        Some(dir) => dir,
        None => std::env::current_dir().context("cannot read the current directory")?,
    };

    let resolved = requested
        .canonicalize()
        .with_context(|| format!("working directory {} is unusable", requested.display()))?;

    if !resolved.is_dir() {
        anyhow::bail!(
            "working directory {} is not a directory",
            resolved.display()
        );
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("mcpd").chain(args.iter().copied()))
    }

    fn file(toml: &str) -> Cli {
        toml::from_str(toml).unwrap()
    }

    fn settled(cli: Cli) -> Settings {
        cli.over(Cli::default()).unwrap()
    }

    #[test]
    fn defaults_apply_when_neither_source_says_otherwise() {
        let settings = settled(flags(&[]));
        assert_eq!(settings.bind, "127.0.0.1:8080");
        assert_eq!(settings.name, "mcpd");
        assert_eq!(settings.list_ttl_ms, 60_000);
        assert!(settings.cwd.is_none());
        assert!(settings.instructions.is_none());
        assert!(settings.title.is_none());
    }

    #[test]
    fn a_display_name_comes_from_either_source() {
        let settings = settled(file(r#"title = "From File""#));
        assert_eq!(settings.title.as_deref(), Some("From File"));

        let settings = flags(&["--title", "From Flag"])
            .over(file(r#"title = "From File""#))
            .unwrap();
        assert_eq!(settings.title.as_deref(), Some("From Flag"));
    }

    #[test]
    fn the_config_file_supplies_what_the_flags_omit() {
        let settings = flags(&[])
            .over(file(
                r#"
                bind = "127.0.0.1:9000"
                name = "from-file"
                list-ttl-ms = 250
                instructions = "be careful"
                "#,
            ))
            .unwrap();

        assert_eq!(settings.bind, "127.0.0.1:9000");
        assert_eq!(settings.name, "from-file");
        assert_eq!(settings.list_ttl_ms, 250);
        assert_eq!(settings.instructions.as_deref(), Some("be careful"));
    }

    #[test]
    fn a_flag_wins_over_the_config_file() {
        let settings = flags(&["--bind", "0.0.0.0:1234", "--name", "from-flag"])
            .over(file(
                r#"
                bind = "127.0.0.1:9000"
                name = "from-file"
                list-ttl-ms = 250
                "#,
            ))
            .unwrap();

        assert_eq!(settings.bind, "0.0.0.0:1234");
        assert_eq!(settings.name, "from-flag");
        assert_eq!(settings.list_ttl_ms, 250);
    }

    #[test]
    fn every_key_is_spelled_the_way_its_flag_is() {
        let settings = settled(file(
            r#"
            bind = "127.0.0.1:1"
            name = "n"
            title = "N"
            cwd = "/tmp"
            instructions = "i"
            list-ttl-ms = 1
            tool = []
            "#,
        ));

        assert_eq!(settings.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn a_tool_may_be_written_as_a_toml_table() {
        let settings = settled(file(
            r#"
            [[tool]]
            name = "sh"

            [tool._meta."dev.subs/exec"]
            argv = ["/bin/sh", "-lc", "{command}"]
            "#,
        ));

        let parsed: serde_json::Value = serde_json::from_str(&settings.tools[0]).unwrap();
        assert_eq!(parsed["name"], "sh");
        assert_eq!(parsed["_meta"]["dev.subs/exec"]["argv"][0], "/bin/sh");
    }

    #[test]
    fn a_tool_may_also_be_the_json_string_the_flag_takes() {
        let settings = settled(file(r#"tool = ['{"name":"sh"}']"#));
        assert_eq!(settings.tools, [r#"{"name":"sh"}"#]);
    }

    #[test]
    fn tools_on_the_command_line_replace_the_files_rather_than_joining_them() {
        let settings = flags(&["--tool", r#"{"name":"flag"}"#])
            .over(file(r#"tool = ['{"name":"file"}']"#))
            .unwrap();

        assert_eq!(settings.tools, [r#"{"name":"flag"}"#]);
    }

    /// The merge is field-agnostic, so what has to be pinned is that every flag
    /// survives it: a value that serializes as `unset` would be silently
    /// dropped, and a key `Settings` does not know would be rejected.
    #[test]
    fn every_flag_carries_its_value_all_the_way_through_the_merge() {
        let typed = keys(&flags(&[
            "--stdio",
            "--no-auth",
            "--bind",
            "127.0.0.1:1",
            "--name",
            "n",
            "--title",
            "N",
            "--mcp-path",
            "/p",
            "--cwd",
            "/tmp",
            "--tool",
            r#"{"name":"t"}"#,
            "--instructions",
            "i",
            "--list-ttl-ms",
            "1",
        ]))
        .unwrap();

        for (key, value) in &typed {
            assert!(!unset(value), "`{key}` was typed but merges as absent");
        }

        let settings: Settings = serde_json::from_value(Value::Object(typed)).unwrap();
        assert_eq!(settings.bind, "127.0.0.1:1");
        assert_eq!(settings.tools, [r#"{"name":"t"}"#]);
        assert!(settings.stdio && settings.no_auth);
    }

    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        let result: Result<Cli, _> = toml::from_str(r#"bnid = "0.0.0.0:1""#);
        assert!(result.is_err());
    }

    #[test]
    fn a_config_file_cannot_name_another_config_file() {
        let result: Result<Cli, _> = toml::from_str(r#"config = "other.toml""#);
        assert!(result.is_err());
    }

    #[test]
    fn http_is_what_you_get_unless_stdio_is_asked_for() {
        assert!(!settled(flags(&[])).stdio);
        assert!(settled(flags(&["--stdio"])).stdio);
    }

    #[test]
    fn a_config_file_can_choose_stdio_too() {
        assert!(flags(&[]).over(file("stdio = true")).unwrap().stdio);
    }

    #[test]
    fn a_flag_that_only_means_something_over_http_is_refused_with_stdio() {
        for conflicting in [
            vec!["--stdio", "--bind", "127.0.0.1:9"],
            vec!["--stdio", "--mcp-path", "/elsewhere"],
            vec!["--stdio", "--no-auth"],
        ] {
            let error = flags(&conflicting).load().unwrap_err().to_string();
            assert!(error.contains(conflicting[1]), "{error}");
            assert!(error.contains("--stdio"), "{error}");
        }
    }

    #[test]
    fn a_config_file_that_binds_a_port_is_no_contradiction() {
        let settings = flags(&["--stdio"])
            .over(file(r#"bind = "127.0.0.1:9000""#))
            .unwrap();

        assert!(settings.stdio);
    }

    #[test]
    fn a_token_is_required_unless_it_is_explicitly_waived() {
        let error = decide(false, None).unwrap_err().to_string();
        assert!(error.contains("MCPD_TOKEN"), "{error}");
        assert!(error.contains("--no-auth"), "{error}");
    }

    #[test]
    fn a_token_is_used_when_one_is_present() {
        assert_eq!(
            decide(false, Some("t".into())).unwrap().as_deref(),
            Some("t")
        );
    }

    #[test]
    fn waiving_authentication_leaves_no_credentials() {
        assert!(decide(true, None).unwrap().is_none());
    }

    #[test]
    fn waiving_authentication_while_supplying_a_token_is_ambiguous() {
        assert!(decide(true, Some("t".into())).is_err());
    }

    #[test]
    fn a_config_file_cannot_turn_authentication_off() {
        let result: Result<Cli, _> = toml::from_str("no-auth = true");
        assert!(result.is_err());
    }

    #[test]
    fn json_says_the_same_things_toml_does() {
        let settings: Cli = serde_json::from_str(
            r#"{
                "bind": "127.0.0.1:9000",
                "name": "from-json",
                "list-ttl-ms": 250,
                "tool": [
                    {
                        "name": "sh",
                        "_meta": { "dev.subs/exec": { "argv": ["/bin/sh"], "timeoutMs": 2000 } }
                    }
                ]
            }"#,
        )
        .unwrap();
        let settings = settled(settings);

        assert_eq!(settings.bind, "127.0.0.1:9000");
        assert_eq!(settings.name, "from-json");
        assert_eq!(settings.list_ttl_ms, 250);

        let parsed: serde_json::Value = serde_json::from_str(&settings.tools[0]).unwrap();
        assert_eq!(parsed["_meta"]["dev.subs/exec"]["timeoutMs"], 2000);
    }

    #[test]
    fn an_unknown_json_key_is_rejected_too() {
        let result: Result<Cli, _> = serde_json::from_str(r#"{"bnid": "0.0.0.0:1"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn a_config_file_needs_an_extension_that_says_how_to_read_it() {
        let error = flags(&["--config", "/tmp/mcpd.conf"])
            .load()
            .unwrap_err()
            .to_string();
        assert!(error.contains(".toml or .json"), "{error}");
    }

    #[test]
    fn the_mcp_endpoint_defaults_to_the_conventional_path() {
        assert_eq!(settled(flags(&[])).mcp_path, "/mcp");
    }

    #[test]
    fn the_mcp_endpoint_can_be_moved_under_a_prefix() {
        let settings = settled(flags(&["--mcp-path", "/agent/mcp"]));
        assert_eq!(mcp_path(settings.mcp_path).unwrap(), "/agent/mcp");
    }

    #[test]
    fn an_mcp_path_without_a_leading_slash_is_rejected() {
        let error = mcp_path("mcp".to_string()).unwrap_err().to_string();
        assert!(error.contains("must start with"), "{error}");
    }

    #[test]
    fn the_mcp_endpoint_cannot_displace_the_health_check() {
        assert!(mcp_path("/health".to_string()).is_err());
    }

    #[test]
    fn a_missing_config_file_is_reported_with_its_path() {
        let error = flags(&["--config", "/no/such/mcpd.toml"])
            .load()
            .unwrap_err()
            .to_string();
        assert!(error.contains("/no/such/mcpd.toml"), "{error}");
    }

    #[test]
    fn an_absent_setting_is_the_daemons_own_directory() {
        let resolved = working_dir(None).unwrap();
        assert_eq!(
            resolved,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn a_configured_directory_is_resolved_to_an_absolute_path() {
        let resolved = working_dir(Some(PathBuf::from("src/../src"))).unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("src"));
    }

    #[test]
    fn a_missing_directory_is_rejected() {
        assert!(working_dir(Some(PathBuf::from("/no/such/place"))).is_err());
    }

    #[test]
    fn a_file_is_not_a_directory() {
        assert!(working_dir(Some(PathBuf::from("Cargo.toml"))).is_err());
    }
}
