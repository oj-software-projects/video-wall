use crate::config::{Config, Grid};

pub const USAGE: &str = "Usage: video-wall [options]";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cli {
    pub dirs: Option<Vec<String>>,
    pub grid: Option<Grid>,
    pub shuffle: Option<bool>,
    pub loop_: Option<bool>,
    pub mute: Option<bool>,
    pub dev: bool,
    pub headless: bool,
    pub help: bool,
}

impl Cli {
    /// Defaults < saved config < CLI. Only keys present in the CLI override.
    pub fn apply(&self, cfg: &mut Config) {
        if let Some(dirs) = &self.dirs {
            cfg.dirs = dirs.clone();
        }
        if let Some(grid) = self.grid {
            cfg.grid = grid;
        }
        if let Some(shuffle) = self.shuffle {
            cfg.shuffle = shuffle;
        }
        if let Some(loop_) = self.loop_ {
            cfg.loop_ = loop_;
        }
        if let Some(mute) = self.mute {
            cfg.mute = mute;
        }
    }
}

fn is_flag(arg: &str) -> bool {
    arg == "-h" || arg.starts_with("--")
}

pub fn parse(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli::default();
    // Electron bootstrap paths precede the real arguments; ignore everything
    // before the first flag.
    let start = args.iter().position(|arg| is_flag(arg)).unwrap_or(args.len());
    let mut i = start;

    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                cli.help = true;
                i += 1;
            }
            "--dev" => {
                cli.dev = true;
                i += 1;
            }
            "--headless" => {
                cli.headless = true;
                i += 1;
            }
            "--shuffle" => {
                cli.shuffle = Some(true);
                i += 1;
            }
            "--no-shuffle" => {
                cli.shuffle = Some(false);
                i += 1;
            }
            "--loop" => {
                cli.loop_ = Some(true);
                i += 1;
            }
            "--mute" => {
                cli.mute = Some(true);
                i += 1;
            }
            "--no-mute" => {
                cli.mute = Some(false);
                i += 1;
            }
            "--dirs" => {
                let mut dirs = Vec::new();
                i += 1;
                while i < args.len() && !is_flag(&args[i]) {
                    dirs.push(args[i].clone());
                    i += 1;
                }
                if dirs.is_empty() {
                    return Err("--dirs requires at least one folder".to_string());
                }
                cli.dirs = Some(crate::config::sanitize_dirs(dirs));
            }
            "--grid" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "--grid requires ROWSxCOLS".to_string())?;
                cli.grid = Some(parse_grid(value)?);
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("Unknown option: {other}"));
            }
            _ => {
                i += 1;
            }
        }
    }

    Ok(cli)
}

pub fn parse_grid(value: &str) -> Result<Grid, String> {
    let invalid = || format!("Invalid grid format: {value} (expected ROWSxCOLS)");
    let (rows, cols) = value
        .split_once(['x', 'X'])
        .ok_or_else(invalid)?;
    let rows: u32 = rows.trim().parse().map_err(|_| invalid())?;
    let cols: u32 = cols.trim().parse().map_err(|_| invalid())?;
    if rows == 0 || cols == 0 {
        return Err(invalid());
    }
    Ok(Grid {
        rows: rows.clamp(1, 8),
        cols: cols.clamp(1, 8),
    })
}

pub fn help_text() -> String {
    format!(
        r#"{USAGE}

Options:
  --dirs <dir1> [dir2...]   Folders to scan; starts the wall immediately
  --grid <ROWSxCOLS>        Grid size, each 1..8 (e.g. 3x2)
  --shuffle / --no-shuffle  Shuffle the selection
  --loop                    Loop each video
  --mute / --no-mute        Start muted / with sound
  --dev                     Open the inspector on start
  --headless                Servers only, no window (testing)
  --help, -h                Show this help

Settings are saved after a successful wall start and overridden by CLI flags
for that run only.

Examples:
  video-wall
  video-wall --dirs ~/Movies ~/Clips --grid 3x2
  video-wall --dirs ~/Movies --no-shuffle --no-mute --loop

Keybindings:
  Tab/Esc  Back to setup
  R        Reshuffle
  P        Pause/Resume
  M        Mute/Unmute
  ?        Toggle Help
  Q        Quit

Supported formats:
  mp4 mkv avi mov webm flv wmv m4v mpg mpeg 3gp ogv
  (files must be at least 1 MiB; hidden ._ resource forks are ignored)
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Cli, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_all_flags() {
        let cli = parse_strs(&[
            "--dirs", "/a", "/b", "--grid", "3X2", "--no-shuffle", "--loop", "--no-mute", "--dev",
        ])
        .unwrap();
        assert_eq!(cli.dirs, Some(vec!["/a".to_string(), "/b".to_string()]));
        assert_eq!(cli.grid, Some(Grid { rows: 3, cols: 2 }));
        assert_eq!(cli.shuffle, Some(false));
        assert_eq!(cli.loop_, Some(true));
        assert_eq!(cli.mute, Some(false));
        assert!(cli.dev);
    }

    #[test]
    fn ignores_args_before_first_flag() {
        let cli = parse_strs(&["/electron/app", "bootstrap.js", "--mute"]).unwrap();
        assert_eq!(cli.mute, Some(true));
        assert_eq!(cli.dirs, None);
    }

    #[test]
    fn rejects_bad_grid_and_unknown_flags() {
        assert!(parse_strs(&["--grid", "3by2"]).is_err());
        assert!(parse_strs(&["--grid", "0x2"]).is_err());
        assert!(parse_strs(&["--nope"]).is_err());
        assert!(parse_strs(&["--dirs"]).is_err());
    }

    #[test]
    fn clamps_grid() {
        assert_eq!(parse_strs(&["--grid", "99x0"]).unwrap_err(), "Invalid grid format: 99x0 (expected ROWSxCOLS)");
        assert_eq!(parse_strs(&["--grid", "99x5"]).unwrap().grid, Some(Grid { rows: 8, cols: 5 }));
    }

    #[test]
    fn merges_only_present_keys() {
        let mut cfg = Config::default();
        cfg.dirs = vec!["/saved".into()];
        cfg.grid = Grid { rows: 4, cols: 4 };
        cfg.shuffle = true;
        let cli = parse_strs(&["--dirs", "/cli", "--no-mute"]).unwrap();
        cli.apply(&mut cfg);
        assert_eq!(cfg.dirs, vec!["/cli".to_string()]);
        assert_eq!(cfg.grid, Grid { rows: 4, cols: 4 });
        assert!(cfg.shuffle);
        assert!(!cfg.mute);
    }

    #[test]
    fn help_lists_required_sections() {
        let help = help_text();
        for needle in ["Usage", "Options", "overridden by CLI", "Examples", "Keybindings", "Supported formats"] {
            assert!(help.contains(needle), "missing {needle}");
        }
    }
}
