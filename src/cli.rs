use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use serde::{Deserialize, Deserializer, de};

use crate::transport::server::{DEFAULT_MCP_PATH, HEALTH_PATH};

pub const TOKEN_ENV: &str = "MCPD_TOKEN";

const DEFAULT_BIND: &str = "0.0.0.0:8080";
const DEFAULT_NAME: &str = "mcpd";
const DEFAULT_LIST_TTL_MS: u64 = 60_000;

/// Both sources of configuration, so a config file cannot drift from the flags:
/// one key per flag, spelled the same. Every field is optional because
/// absent has to stay distinguishable from default until the two are merged.
#[derive(Parser, Deserialize, Debug, Default)]
#[command(
    name = "mcpd",
    version,
    about = "Serve MCP over HTTP. Every tool is a command on this machine."
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
    #[serde(skip)]
    pub no_auth: bool,

    #[arg(
        long,
        value_name = "ADDR",
        help = "Listen address [default: 0.0.0.0:8080]"
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

/// What the two sources agreed on, with defaults filled in.
#[derive(Debug)]
pub struct Settings {
    pub no_auth: bool,
    pub bind: String,
    pub name: String,
    pub mcp_path: String,
    pub cwd: Option<PathBuf>,
    pub tools: Vec<String>,
    pub instructions: Option<String>,
    pub list_ttl_ms: u64,
}

impl Cli {
    pub fn load(self) -> anyhow::Result<Settings> {
        let file = match self.config.as_deref() {
            Some(path) => Some(read_config(path)?),
            None => None,
        };

        Ok(match file {
            Some(file) => self.over(file).resolve(),
            None => self.resolve(),
        })
    }

    fn over(self, file: Cli) -> Cli {
        Cli {
            config: self.config,
            no_auth: self.no_auth,
            bind: self.bind.or(file.bind),
            name: self.name.or(file.name),
            mcp_path: self.mcp_path.or(file.mcp_path),
            cwd: self.cwd.or(file.cwd),
            tools: if self.tools.is_empty() {
                file.tools
            } else {
                self.tools
            },
            instructions: self.instructions.or(file.instructions),
            list_ttl_ms: self.list_ttl_ms.or(file.list_ttl_ms),
        }
    }

    fn resolve(self) -> Settings {
        Settings {
            no_auth: self.no_auth,
            bind: self.bind.unwrap_or_else(|| DEFAULT_BIND.to_string()),
            name: self.name.unwrap_or_else(|| DEFAULT_NAME.to_string()),
            mcp_path: self
                .mcp_path
                .unwrap_or_else(|| DEFAULT_MCP_PATH.to_string()),
            cwd: self.cwd,
            tools: self.tools,
            instructions: self.instructions,
            list_ttl_ms: self.list_ttl_ms.unwrap_or(DEFAULT_LIST_TTL_MS),
        }
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

    #[test]
    fn defaults_apply_when_neither_source_says_otherwise() {
        let settings = flags(&[]).resolve();
        assert_eq!(settings.bind, "0.0.0.0:8080");
        assert_eq!(settings.name, "mcpd");
        assert_eq!(settings.list_ttl_ms, 60_000);
        assert!(settings.cwd.is_none());
        assert!(settings.instructions.is_none());
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
            .resolve();

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
            .resolve();

        assert_eq!(settings.bind, "0.0.0.0:1234");
        assert_eq!(settings.name, "from-flag");
        assert_eq!(settings.list_ttl_ms, 250);
    }

    #[test]
    fn every_key_is_spelled_the_way_its_flag_is() {
        let settings = file(
            r#"
            bind = "127.0.0.1:1"
            name = "n"
            cwd = "/tmp"
            instructions = "i"
            list-ttl-ms = 1
            tool = []
            "#,
        )
        .resolve();

        assert_eq!(settings.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn a_tool_may_be_written_as_a_toml_table() {
        let settings = file(
            r#"
            [[tool]]
            name = "sh"

            [tool._meta."dev.subs/exec"]
            argv = ["/bin/sh", "-lc", "{command}"]
            "#,
        )
        .resolve();

        let parsed: serde_json::Value = serde_json::from_str(&settings.tools[0]).unwrap();
        assert_eq!(parsed["name"], "sh");
        assert_eq!(parsed["_meta"]["dev.subs/exec"]["argv"][0], "/bin/sh");
    }

    #[test]
    fn a_tool_may_also_be_the_json_string_the_flag_takes() {
        let settings = file(r#"tool = ['{"name":"sh"}']"#).resolve();
        assert_eq!(settings.tools, [r#"{"name":"sh"}"#]);
    }

    #[test]
    fn tools_on_the_command_line_replace_the_files_rather_than_joining_them() {
        let settings = flags(&["--tool", r#"{"name":"flag"}"#])
            .over(file(r#"tool = ['{"name":"file"}']"#))
            .resolve();

        assert_eq!(settings.tools, [r#"{"name":"flag"}"#]);
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
        let settings = settings.resolve();

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
        assert_eq!(flags(&[]).resolve().mcp_path, "/mcp");
    }

    #[test]
    fn the_mcp_endpoint_can_be_moved_under_a_prefix() {
        let settings = flags(&["--mcp-path", "/agent/mcp"]).resolve();
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
