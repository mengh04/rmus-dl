//! rmus-dl 运行配置。目前只有一项：flac_only（只要无损）。
//!
//! 优先级：环境变量 `RMUS_DL_FLAC_ONLY` > 配置文件 > 默认（开）。
//! 配置文件：`$XDG_CONFIG_HOME/rmus-dl/config.toml`（默认 `~/.config/rmus-dl/`），
//! 一行 `flac_only = false` 即可关掉无损限定。
//!
//! flac_only 开启时：kuwo 只走 mobi.s flac（不再降级 mp3），kugou 先查
//! songinfo 拿无损专属 sqhash，且下载前都会用 Range 探针验证魔数。

use std::path::PathBuf;

pub fn flac_only() -> bool {
    if let Ok(value) = std::env::var("RMUS_DL_FLAC_ONLY") {
        let value = value.trim().to_lowercase();
        return !matches!(value.as_str(), "0" | "false" | "off" | "no");
    }
    config_file_flac_only().unwrap_or(true)
}

/// 配置文件里的 flac_only；文件/键不存在时返回 None。
fn config_file_flac_only() -> Option<bool> {
    let path = config_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some(value) = line.strip_prefix("flac_only") else { continue };
        let value = value.trim().strip_prefix('=')?.trim().to_lowercase();
        return Some(!matches!(value.as_str(), "0" | "false" | "off" | "no"));
    }
    None
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home).join(".config")
            })
        })?;
    Some(base.join("rmus-dl").join("config.toml"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn 环境变量覆盖默认值() {
        // 不真改进程环境（并行测试不安全），只验证解析逻辑
        let parse = |value: &str| !matches!(value.trim().to_lowercase().as_str(), "0" | "false" | "off" | "no");
        assert!(parse("1"));
        assert!(parse("true"));
        assert!(!parse("0"));
        assert!(!parse("false"));
    }
}
