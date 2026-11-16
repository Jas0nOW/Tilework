//! User settings, stored as a plain `key=value` file in `%LOCALAPPDATA%\CosmicTile\config.ini`.
//! Unknown keys and broken lines are ignored so an old or hand-edited file never stops the app.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Hovering a tiled window for `focus_delay_ms` gives it the keyboard focus.
    pub focus_follows_mouse: bool,
    pub focus_delay_ms: u64,
    /// Smallest zone a window may be squeezed into (keeps part of the title bar grabbable).
    pub min_zone_width: i32,
    pub min_zone_height: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self { focus_follows_mouse: true, focus_delay_ms: 300, min_zone_width: 260, min_zone_height: 160 }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
        base.join("CosmicTile").join("config.ini")
    }

    pub fn parse(text: &str) -> Self {
        let mut c = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "focus_follows_mouse" => {
                    if let Ok(b) = v.parse() {
                        c.focus_follows_mouse = b;
                    }
                }
                "focus_delay_ms" => {
                    if let Ok(n) = v.parse::<u64>() {
                        c.focus_delay_ms = n.clamp(50, 5000);
                    }
                }
                "min_zone_width" => {
                    if let Ok(n) = v.parse::<i32>() {
                        c.min_zone_width = n.clamp(80, 1000);
                    }
                }
                "min_zone_height" => {
                    if let Ok(n) = v.parse::<i32>() {
                        c.min_zone_height = n.clamp(60, 1000);
                    }
                }
                _ => {}
            }
        }
        c
    }

    pub fn to_text(&self) -> String {
        format!(
            "# CosmicTile settings (changes made here apply after restarting CosmicTile)\n\
             # Hover a window for focus_delay_ms to give it the keyboard focus.\n\
             focus_follows_mouse={}\n\
             focus_delay_ms={}\n\
             # Smallest zone a window can be squeezed into (pixels).\n\
             min_zone_width={}\n\
             min_zone_height={}\n",
            self.focus_follows_mouse, self.focus_delay_ms, self.min_zone_width, self.min_zone_height
        )
    }

    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(t) => Self::parse(&t),
            Err(_) => {
                let c = Self::default();
                c.save(); // first start: write the defaults so the file can be found and edited
                c
            }
        }
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, self.to_text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_mouse_after_300_ms() {
        let c = Config::default();
        assert!(c.focus_follows_mouse);
        assert_eq!(c.focus_delay_ms, 300);
    }

    #[test]
    fn a_saved_config_reads_back_the_same() {
        let c = Config { focus_follows_mouse: false, focus_delay_ms: 800, min_zone_width: 300, min_zone_height: 200 };
        assert_eq!(Config::parse(&c.to_text()), c);
    }

    #[test]
    fn broken_lines_and_extreme_values_are_tolerated() {
        let c = Config::parse("garbage\nfocus_delay_ms=abc\nfocus_delay_ms=999999\nmin_zone_width=1\n# comment\nunknown=1\n");
        assert_eq!(c.focus_delay_ms, 5000);
        assert_eq!(c.min_zone_width, 80);
        assert!(c.focus_follows_mouse);
    }
}
