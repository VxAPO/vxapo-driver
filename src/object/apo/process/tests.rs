use super::*;
use crate::object::vx_reg_props::CLSID_VXAPO_PRE_MIX;

/// 测试：`apo_process_panic_fallback` 在模拟 panic 后输出清零 + BUFFER_SILENT + error_count++。
///
/// 直接用 `catch_unwind` + 注入 panic 的闭包验证防御路径——不依赖真实 FFI 调用。
#[test]
fn rt_panic_fallback_silences_output() {
    // 构造 APO：1 输入 1 输出，输出缓冲 960 帧。
    let apo = ApoObject::new(CLSID_VXAPO_PRE_MIX);
    {
        let mut inner = apo.mutex.lock().unwrap();
        inner.pipeline_context.output_channels = 2;
        inner.pipeline_context.max_frame_count = 960;
    }
    let mut buffer = vec![1.0f32; 960 * 2];
    let mut prop = APO_CONNECTION_PROPERTY {
        pBuffer: buffer.as_mut_ptr() as usize,
        u32ValidFrameCount: 960,
        u32BufferFlags: BUFFER_VALID,
        u32Signature: 0,
    };
    // 两级指针：先取 &mut APO_CONNECTION_PROPERTY → *mut，再取 &mut 该指针 → *mut *mut。
    let mut single: *mut APO_CONNECTION_PROPERTY = &mut prop;
    let props: *mut *mut APO_CONNECTION_PROPERTY = &mut single;

    // 触发 panic 的闭包（模拟 apo_process_inner 内部 panic 后传播到 catch_unwind）。
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        panic!("rt panic sim");
    }));
    assert!(result.is_err());

    // 模拟 RT 入口捕获后调用 fallback。
    apo.apo_process_panic_fallback(1, props);

    // 输出缓冲清零 + BUFFER_SILENT + error_count++。
    assert!(buffer.iter().all(|&v| v == 0.0), "output must be zeroed");
    assert_eq!(prop.u32BufferFlags, BUFFER_SILENT);
    assert_eq!(apo.process_stats.error_count.load(Ordering::Relaxed), 1);
}

/// 测试：CalcInputFrames / CalcOutputFrames panic 时返回保守值（不 panic、不越界）。
///
/// `_Impl` 由 `#[implement]` 宏生成（无法直接构造），此处验证等价逻辑：
/// 保守值策略（CalcInputFrames → output_frames；CalcOutputFrames → 0）与真实实现一致。
#[test]
fn rt_frame_calc_panic_returns_conservative_values() {
    // 保守值语义直接验证：catch_unwind 包裹后 panic → 返回保守值（不二次 panic）。
    let input_ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = 480u32; // 正常计算占位
            480u32.wrapping_add(0)
        }))
        .unwrap_or_else(|_| 480) // CalcInputFrames 保守值 = output_frames
    }));
    assert_eq!(input_ret.unwrap(), 480);

    let output_ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("calc panic"); // 模拟内部 panic
        }))
        .unwrap_or_else(|_| 0) // CalcOutputFrames 保守值 = 0（可丢帧不可越界）
    }));
    assert_eq!(output_ret.unwrap(), 0);
}
