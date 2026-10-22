//! Command line parsing, swaybg-compatible plus `--dir`/`--time`.

use std::{path::PathBuf, process::exit, time::Duration};

use crate::render::Mode;

pub const USAGE: &str = "Usage: reel <options...>

  -i, --image <file>     Set the image to display.
  -d, --dir <dir>        Cycle through the images in a directory.
  -t, --time <time>      Interval for --dir: 30s, 5m, 1h.
  -s, --shuffle          Show the images of --dir in random order.
  -m, --mode <mode>      Set the mode to use for the image (default: fill).
  -c, --color RRGGBB     Set the background color.
  -r, --rotate <degrees> Rotate the image clockwise: 0, 90, 180 or 270.
  -o, --output <name>    Set the output to operate on or * for all.
  -h, --help             Show help message and quit.
  -v, --version          Show the version number and quit.

Options before the first -o apply to all outputs (*).

Background Modes:
  stretch, fit, fill, center, tile, or solid_color";

/// Command line settings for one `-o` block.
#[derive(Default)]
pub struct Config {
    pub output: String,
    pub image: Option<PathBuf>,
    pub dir: Option<PathBuf>,
    pub interval: Option<Duration>,
    pub mode: Option<Mode>,
    pub color: Option<[u8; 3]>,
    /// Clockwise rotation in degrees: 0, 90, 180 or 270.
    pub rotate: Option<u32>,
    pub shuffle: bool,
}

/// Parses `30s`, `5m`, `1h` or a bare number of seconds.
fn parse_duration(s: &str) -> Option<Duration> {
    let (num, mult) = match s.as_bytes().last()? {
        b's' => (&s[..s.len() - 1], 1),
        b'm' => (&s[..s.len() - 1], 60),
        b'h' => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    let secs = num.parse::<u64>().ok()?.checked_mul(mult)?;
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Parses `rrggbb` or `#rrggbb`.
pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn die(msg: &str) -> ! {
    eprintln!("reel: {msg}\n\n{USAGE}");
    exit(1)
}

/// Adds `cfg` to `configs`, merging it on top of an earlier block for the same output.
fn store(configs: &mut Vec<Config>, cfg: Config) {
    let Some(old) = configs.iter_mut().find(|c| c.output == cfg.output) else {
        return configs.push(cfg);
    };
    if cfg.image.is_some() || cfg.dir.is_some() {
        (old.image, old.dir) = (cfg.image, cfg.dir);
    }
    old.interval = cfg.interval.or(old.interval);
    old.mode = cfg.mode.or(old.mode);
    old.color = cfg.color.or(old.color);
    old.rotate = cfg.rotate.or(old.rotate);
    old.shuffle |= cfg.shuffle;
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Vec<Config> {
    let mut configs = Vec::new();
    let mut cfg = Config {
        output: "*".into(),
        ..Default::default()
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| die(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "-i" | "--image" => cfg.image = Some(PathBuf::from(value())),
            "-d" | "--dir" => cfg.dir = Some(PathBuf::from(value())),
            "-t" | "--time" => {
                let v = value();
                cfg.interval =
                    Some(parse_duration(&v).unwrap_or_else(|| die(&format!("invalid time: {v}"))));
            }
            "-m" | "--mode" => {
                let v = value();
                cfg.mode = Some(
                    v.parse::<Mode>()
                        .ok()
                        .unwrap_or_else(|| die(&format!("invalid mode: {v}"))),
                );
            }
            "-c" | "--color" => {
                let v = value();
                cfg.color = Some(parse_color(&v).unwrap_or_else(|| {
                    die(&format!("invalid color: {v}, expected rrggbb or #rrggbb"))
                }));
            }
            "-s" | "--shuffle" => cfg.shuffle = true,
            "-r" | "--rotate" => {
                let v = value();
                cfg.rotate = Some(
                    v.parse()
                        .ok()
                        .filter(|d| [0, 90, 180, 270].contains(d))
                        .unwrap_or_else(|| {
                            die(&format!(
                                "invalid rotation: {v}, expected 0, 90, 180 or 270"
                            ))
                        }),
                );
            }
            "-o" | "--output" => {
                let output = value();
                store(
                    &mut configs,
                    std::mem::replace(
                        &mut cfg,
                        Config {
                            output,
                            ..Default::default()
                        },
                    ),
                );
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0)
            }
            "-v" | "--version" => {
                println!("reel version {}", env!("CARGO_PKG_VERSION"));
                exit(0)
            }
            _ => die(&format!("unknown argument: {arg}")),
        }
    }
    store(&mut configs, cfg);

    // Like swaybg, a block without an image or a color does nothing.
    configs.retain(|c| c.image.is_some() || c.dir.is_some() || c.color.is_some());
    if configs.is_empty() {
        die("one of --image, --dir or --color is required");
    }
    for c in &configs {
        let out = &c.output;
        match (&c.image, &c.dir) {
            (Some(_), Some(_)) => die(&format!(
                "output {out}: --image and --dir are mutually exclusive"
            )),
            (Some(f), _) if !f.is_file() => {
                die(&format!("output {out}: {} is not a file", f.display()))
            }
            (_, Some(d)) if !d.is_dir() => {
                die(&format!("output {out}: {} is not a directory", d.display()))
            }
            _ => {}
        }
        if c.dir.is_some() != c.interval.is_some() {
            die(&format!("output {out}: --dir and --time go together"));
        }
        if c.shuffle && c.dir.is_none() {
            die(&format!("output {out}: --shuffle needs --dir"));
        }
    }
    configs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        for bad in ["", "m", "0m", "-5m", "5d", "1.5m"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_color("00ff00"), Some([0, 255, 0]));
        for bad in ["", "#fff", "gg0000", "+ff000", "ff00000"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn output_blocks() {
        let c = parse_args(args(
            "-c 000000 -o DP-1 -c ffffff -m fit -o * -m tile -o DP-1 -c 00ff00",
        ));
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].output.as_str(), c[0].color, c[0].mode),
            ("*", Some([0, 0, 0]), Some(Mode::Tile))
        );
        assert_eq!(
            (c[1].output.as_str(), c[1].color, c[1].mode),
            ("DP-1", Some([0, 255, 0]), Some(Mode::Fit))
        );
        // A block with neither image nor color is dropped.
        assert_eq!(parse_args(args("-c 000000 -o DP-2 -m fit")).len(), 1);
    }

    #[test]
    fn rotation_merges_per_output() {
        let c = parse_args(args("-c 000000 -r 90 -o DP-1 -c ffffff -o * -r 270"));
        assert_eq!((c[0].output.as_str(), c[0].rotate), ("*", Some(270)));
        assert_eq!((c[1].output.as_str(), c[1].rotate), ("DP-1", None));
        assert!(!c[0].shuffle && !c[1].shuffle);
    }
}
