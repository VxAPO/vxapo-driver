//! dsp/filters/vst.rs — VST 插件加载（feature gate，初期占位，Note 54）
//!
//! 实现 `VSTPlugin:` 命令。
//!
//! 当前为占位实现：参数解析 + passthrough。
//! 完整 VST2/VST3 支持需要：
//! - `vst2` 或 `vst3` feature gate
//! - 动态库加载（`LoadLibraryW`）
//! - `AEffect` / `IPluginFactory` COM 接口
//! - 参数自动化映射
//!
//! 非目标（不支持）：
//! - VST GUI
//! - MIDI 通路
//! - 多输出通道

use crate::pipeline::dsp::filter::Filter;

/// VST 插件滤波器（占位）。
#[derive(Debug)]
pub struct VstFilter {
    /// 插件 DLL 路径。
    dll_path: String,
    /// 插件名称（日志用）。
    plugin_name: String,
    /// 通道数。
    num_channels: usize,
}

impl VstFilter {
    /// 创建 VST 滤波器。
    pub fn new(dll_path: &str, plugin_name: &str) -> Self {
        Self {
            dll_path: dll_path.to_owned(),
            plugin_name: plugin_name.to_owned(),
            num_channels: 0,
        }
    }
}

impl Filter for VstFilter {
    fn initialize(&mut self, _sample_rate: u32, channel_names: &[String]) -> Option<Vec<String>> {
        self.num_channels = channel_names.len().max(1);

        // TODO: LoadLibraryW + AEffect 创建
        log::warn!(
            "VSTPlugin: not yet implemented (dll='{}', name='{}'). Passing through.",
            self.dll_path, self.plugin_name
        );

        None
    }

    fn process(&mut self, _samples: &mut [Vec<f32>], _frame_count: usize) {
        // 占位 passthrough
    }
}

/// 解析 `VSTPlugin:` 参数。
///
/// 格式：`"plugin_name" "path/to/plugin.dll" [param1=value1 ...]`
///
/// 返回 `(plugin_name, dll_path, raw_params)`。
pub fn parse_vst_params(params: &str) -> Option<(String, String, String)> {
    let params = params.trim();
    if params.is_empty() {
        return None;
    }

    // 简单解析：按引号分割
    let mut parts = Vec::new();
    let mut remaining = params;

    while !remaining.is_empty() {
        remaining = remaining.trim_start();
        if remaining.starts_with('"') {
            // 带引号的参数
            if let Some(end) = remaining[1..].find('"') {
                parts.push(remaining[1..end + 1].to_owned());
                remaining = &remaining[end + 2..];
            } else {
                // 未闭合引号——取剩余全部
                parts.push(remaining[1..].to_owned());
                remaining = "";
            }
        } else {
            // 不带引号的参数
            if let Some(end) = remaining.find(char::is_whitespace) {
                parts.push(remaining[..end].to_owned());
                remaining = &remaining[end..];
            } else {
                parts.push(remaining.to_owned());
                remaining = "";
            }
        }
    }

    if parts.len() < 2 {
        return None;
    }

    let plugin_name = parts[0].clone();
    let dll_path = parts[1].clone();
    let raw_params = if parts.len() > 2 {
        parts[2..].join(" ")
    } else {
        String::new()
    };

    Some((plugin_name, dll_path, raw_params))
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_names() -> Vec<String> {
        vec!["L".into(), "R".into()]
    }

    // ── parse_vst_params ────────────────────────────────────────────────────

    #[test]
    fn parse_quoted() {
        let (name, path, raw) = parse_vst_params("\"MyPlugin\" \"C:\\VST\\plugin.dll\"").unwrap();
        assert_eq!(name, "MyPlugin");
        assert_eq!(path, "C:\\VST\\plugin.dll");
        assert!(raw.is_empty());
    }

    #[test]
    fn parse_with_extra_params() {
        let (name, path, raw) =
            parse_vst_params("\"MyPlugin\" \"plugin.dll\" gain=6 mix=0.5").unwrap();
        assert_eq!(name, "MyPlugin");
        assert_eq!(path, "plugin.dll");
        assert_eq!(raw, "gain=6 mix=0.5");
    }

    #[test]
    fn parse_empty() {
        assert!(parse_vst_params("").is_none());
    }

    #[test]
    fn parse_single_part() {
        assert!(parse_vst_params("plugin.dll").is_none());
    }

    // ── VstFilter ───────────────────────────────────────────────────────────

    #[test]
    fn passthrough_preserves_signal() {
        let mut filter = VstFilter::new("plugin.dll", "TestPlugin");
        filter.initialize(48000, &stereo_names());

        let mut samples = vec![vec![1.0, 2.0, 3.0], vec![0.5, 1.0, 1.5]];
        let input = samples.clone();
        filter.process(&mut samples, 3);

        assert_eq!(samples, input);
    }

    #[test]
    fn latency_zero() {
        let filter = VstFilter::new("plugin.dll", "TestPlugin");
        assert_eq!(filter.latency(), 0);
    }
}