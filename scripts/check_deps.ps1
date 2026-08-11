# check_deps.ps1 — vxapo-driver 依赖铁律自动校验（v9.17）
#
# 与主规范「模块引用规范（无详细模块版）.md」第十一节「引用约束总表」
# 三文件联动同步维护：总表修订时，本脚本的 $Allow 白名单必须同步更新。
#
# 规则：
# - 解析各 src/*.rs 的 `use crate::` / `crate::` / `super::` 引用（忽略
#   `#[cfg(test)]` 与 `mod tests` 测试块、注释、字符串）；
# - 每个文件按白名单断言，允许条目为前缀匹配（`crate::pipeline` 匹配其所有子路径）；
# - `crate::`（全量）仅 `object/apo.rs` 胶水入口使用；
# - 未列入白名单的文件按「零 crate 内部引用」检查（模块声明根文件）。
#
# 用法：scripts\check_deps.ps1 [vxapo-driver 根目录]
# 退出码：0 = 通过；1 = 存在违规；2 = 路径错误。

$ErrorActionPreference = 'Stop'

$DriverRoot = if ($args.Count -gt 0) {
    $args[0]
} else {
    (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}
$SrcDir = Join-Path $DriverRoot 'src'
if (-not (Test-Path $SrcDir)) {
    Write-Error "src 目录不存在：$SrcDir"
    exit 2
}

# ── 白名单（相对 src/ 的路径 → 允许的 crate:: 前缀）──
# 同步来源：主规范第十一节（v9.17）。条目为前缀匹配；空数组 = 禁止任何 crate 内部引用。
$Allow = @{
    'sys/audio_defs.rs'          = @()
    'sys/com/prelude.rs'         = @()
    'sys/com/apo_interfaces.rs'  = @('crate::sys::com::prelude', 'crate::sys::com::apo_types')
    'sys/com/apo_types.rs'       = @('crate::sys::com::prelude')
    'sys/registry.rs'            = @('crate::sys::com::prelude')
    'sys/known_folder.rs'        = @()

    'pipeline/context.rs'            = @()
    'pipeline/buffer.rs'             = @('crate::sys::com::apo_types')
    'pipeline/format.rs'             = @(
        'crate::sys::com::apo_interfaces', 'crate::sys::com::apo_types',
        'crate::sys::com::prelude', 'crate::sys::audio_defs', 'crate::utils::vx_error'
    )
    'pipeline/interleave.rs'         = @()
    'pipeline/chain.rs'              = @('crate::pipeline::dsp::filter', 'crate::utils')
    'pipeline/process.rs'            = @(
        'crate::pipeline::context', 'crate::pipeline::chain', 'crate::pipeline::buffer',
        'crate::pipeline::interleave', 'crate::pipeline::dsp::filter',
        'crate::pipeline::dsp::transition', 'crate::pipeline::realtime::contract',
        'crate::sys::com::apo_types', 'crate::utils'
    )
    'pipeline/realtime/contract.rs'  = @('crate::rt_assert_not_in_rt')   # #[macro_export] 宏
    'pipeline/realtime/ring.rs'      = @()
    'pipeline/dsp/filter.rs'         = @('crate::utils')
    'pipeline/dsp/factory.rs'        = @(
        'crate::pipeline::dsp::filter', 'crate::pipeline::dsp::model',
        'crate::pipeline::dsp', 'crate::utils'
    )
    'pipeline/dsp/transition.rs'     = @()
    # 具体滤波器：允许 dsp 兄弟协作（总注 v9.17：dsp/math、dsp/fir 等）。
    'pipeline/dsp/aural.rs'          = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/biquad.rs'         = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/fir.rs'            = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/gain.rs'           = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/loudness.rs'       = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/math.rs'           = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/maximizer.rs'      = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/model.rs'          = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/peq_hybrid.rs'     = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/reverb.rs'         = @('crate::pipeline::dsp', 'crate::utils')
    'pipeline/dsp/wide.rs'           = @('crate::pipeline::dsp', 'crate::utils')

    'install/device/endpoint.rs'     = @(
        'crate::sys::registry', 'crate::sys::com::prelude', 'crate::utils'
    )
    'install/device/format.rs'       = @('crate::sys::registry', 'crate::utils', 'crate::sys::audio_defs')
    'install/device/slots.rs'        = @('crate::sys::registry', 'crate::utils::guid', 'crate::sys::com::prelude')
    'install/device/info.rs'         = @(
        'crate::install::device::endpoint', 'crate::install::device::format',
        'crate::install::device::slots', 'crate::sys::registry',
        'crate::object::vx_reg_props', 'crate::utils::vx_error'
    )
    'install/device/sysfx.rs'        = @(
        'crate::install::device::slots', 'crate::object::vx_reg_props',
        'crate::sys::registry', 'crate::sys::com::prelude', 'crate::utils::vx_error'
    )
    'install/selector.rs'            = @('crate::install::selector::select', 'crate::install::selector::operation')
    'install/selector/select.rs'     = @(
        'crate::install::device::info', 'crate::install::device::slots',
        'crate::install::selector::operation', 'crate::utils::vx_error'
    )
    'install/selector/operation.rs'  = @(
        'crate::install::audiodg', 'crate::install::device::slots',
        'crate::install::device::sysfx', 'crate::install::device::format',
        'crate::sys::registry', 'crate::object::vx_reg_props',
        'crate::object::dll_exports', 'crate::sys::com::prelude', 'crate::utils::vx_error'
    )
    'install/audiodg.rs'             = @('crate::sys::registry', 'crate::utils::vx_error')

    'config/error.rs'                = @()
    'config/model.rs'                = @(
        'crate::config::error', 'crate::pipeline::dsp::model',
        'crate::pipeline::dsp::aural', 'crate::pipeline::dsp::maximizer',
        'crate::pipeline::dsp::reverb', 'crate::pipeline::dsp::wide'
    )
    'config/parser.rs'               = @(
        'crate::config::error', 'crate::config::model', 'crate::pipeline::dsp::model',
        'crate::pipeline::dsp::filter', 'crate::pipeline::dsp::factory'
    )
    'config/watcher.rs'              = @('crate::config::error', 'crate::utils')

    'object/apo.rs'                  = @('crate::')   # 胶水入口：允许所有模块
    'object/apo/aggregate.rs'        = @(
        'crate::object::apo', 'crate::sys::com::prelude', 'crate::sys::com::apo_interfaces'
    )
    'object/apo/child.rs'            = @(
        'crate::sys::com::prelude', 'crate::sys::com::apo_interfaces', 'crate::sys::com::apo_types'
    )
    'object/apo/config.rs'           = @(
        'crate::config::parser', 'crate::config::watcher', 'crate::pipeline::chain',
        'crate::pipeline::dsp::transition', 'crate::sys::com::apo_types',
        'crate::sys::com::prelude', 'crate::object::apo', 'crate::object::apo::inner'
    )
    'object/apo/init.rs'             = @(
        'crate::install::device::slots', 'crate::install::device::sysfx',
        'crate::install::selector::operation', 'crate::object::vx_reg_props',
        'crate::object::apo', 'crate::object::apo::aggregate', 'crate::object::apo::config',
        'crate::object::apo::state',
        'crate::object::apo::child', 'crate::sys::com::prelude', 'crate::sys::com::apo_types'
    )
    'object/apo/inner.rs'            = @(
        'crate::pipeline::chain', 'crate::pipeline::context', 'crate::pipeline::dsp::filter',
        'crate::pipeline::dsp::transition', 'crate::sys::audio_defs'
    )
    'object/apo/negotiate.rs'        = @(
        'crate::sys::com::apo_interfaces', 'crate::sys::com::apo_types',
        'crate::pipeline::format', 'crate::utils'
    )
    'object/apo/process.rs'          = @(
        'crate::config::parser', 'crate::install::audiodg', 'crate::object::vx_reg_props',
        'crate::object::apo', 'crate::pipeline', 'crate::sys::audio_defs',
        'crate::sys::com::apo_interfaces', 'crate::sys::com::apo_types', 'crate::sys::com::prelude'
    )
    'object/apo/state.rs'            = @('crate::sys::com::apo_types', 'crate::sys::com::prelude')
    'object/factory.rs'              = @(
        'crate::sys::com::prelude', 'crate::object::apo::aggregate',
        'crate::object::vx_reg_props', 'crate::object::ref_count'
    )
    'object/ref_count.rs'            = @()
    'object/vx_reg_props.rs'         = @(
        'crate::sys::com::prelude', 'crate::sys::com::apo_types', 'crate::sys::com::apo_interfaces'
    )
    'object/dll_exports.rs'          = @(
        'crate::object', 'crate::sys::com::prelude', 'crate::sys::registry',
        'crate::utils', 'crate::telemetry'
    )

    'utils/align.rs'                 = @()
    'utils/guid.rs'                  = @()
    'utils/vx_error.rs'              = @()

    'telemetry/logger.rs'            = @('crate::pipeline::realtime::ring')
    'telemetry/panic.rs'             = @('crate::telemetry::logger')
}

# ── 取非测试代码行（跳过 cfg(test)/mod tests 块与注释）──
function Get-CodeLines {
    param([string[]]$Lines)
    $code = [System.Collections.Generic.List[string]]::new()
    $skipDepth = 0
    $expectItem = $false
    $inComment = $false
    foreach ($raw in $Lines) {
        $line = $raw
        if ($inComment) {
            $end = $line.IndexOf('*/')
            if ($end -lt 0) { continue }
            $line = $line.Substring($end + 2)
            $inComment = $false
        }
        $line = [regex]::Replace($line, '/\*.*?\*/', '')
        if ($line -match '/\*') {
            $inComment = $true
            $line = $line.Substring(0, $line.IndexOf('/*'))
        }
        $t = $line.Trim()
        if ($t -eq '' -or $t.StartsWith('//')) { continue }

        if ($skipDepth -gt 0) {
            $skipDepth += ([regex]::Matches($line, '\{')).Count
            $skipDepth -= ([regex]::Matches($line, '\}')).Count
            if ($skipDepth -le 0) { $skipDepth = 0 }
            continue
        }
        if ($expectItem) {
            if ($t -match '\{') {
                $skipDepth = 1 + ([regex]::Matches($line, '\{')).Count - ([regex]::Matches($line, '\}')).Count
                if ($skipDepth -le 0) { $skipDepth = 0; $expectItem = $false }
            }
            continue
        }
        if ($t -match '^#\[cfg\(test\)\]') { $expectItem = $true; continue }
        if ($t -match '^mod tests\b') {
            if ($t -match '\{') {
                $skipDepth = 1 + ([regex]::Matches($line, '\{')).Count - ([regex]::Matches($line, '\}')).Count
                if ($skipDepth -le 0) { $skipDepth = 0 }
            }
            continue
        }
        $code.Add($line)
    }
    return ,$code
}

# ── 提取 crate:: / super:: 引用并解析为 crate 路径 ──
function Get-Refs {
    param([string]$Line, [string[]]$BaseSegs)
    $refs = [System.Collections.Generic.List[string]]::new()
    foreach ($m in [regex]::Matches($Line, '\bcrate::[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*')) {
        $refs.Add($m.Value)
    }
    $rootBrace = [regex]::Match($Line, 'use\s+crate::\{([^}]*)\}')
    if ($rootBrace.Success) {
        foreach ($part in $rootBrace.Groups[1].Value.Split(',')) {
            $p = $part.Trim()
            if ($p -match '^[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*') {
                $refs.Add('crate::' + $Matches[0])
            }
        }
    }
    foreach ($m in [regex]::Matches($Line, '\bsuper::[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*')) {
        $path = $m.Value
        $suffix = $path
        $ups = 0
        while ($suffix.StartsWith('super::')) {
            $ups++
            $suffix = $suffix.Substring(7)
        }
        $segs = [System.Collections.Generic.List[string]]::new()
        $drop = $ups - 1
        for ($i = 0; $i -lt $BaseSegs.Count - $drop; $i++) {
            $segs.Add($BaseSegs[$i])
        }
        foreach ($s in $suffix.Split('::')) {
            if ($s -ne '') { $segs.Add($s) }
        }
        $refs.Add('crate::' + ($segs -join '::'))
    }
    return ,$refs
}

$violations = 0
$checked = 0
foreach ($f in (Get-ChildItem -Path $SrcDir -Recurse -Filter '*.rs' | Sort-Object FullName)) {
    $rel = $f.FullName.Substring($SrcDir.Length + 1).Replace('\', '/')
    $allowed = $Allow[$rel]
    if ($null -eq $allowed) { $allowed = @() }
    $lines = Get-Content -LiteralPath $f.FullName -Encoding UTF8
    $code = Get-CodeLines $lines
    $dirParts = $rel.Split('/')
    $baseSegs = @()
    for ($i = 0; $i -lt $dirParts.Count - 1; $i++) { $baseSegs += $dirParts[$i] }
    # 文件自身模块路径（用于豁免自引用，如 contract.rs 内部引用自身模块函数）。
    $ownParts = @()
    for ($i = 0; $i -lt $dirParts.Count - 1; $i++) { $ownParts += $dirParts[$i] }
    $ownParts += [System.IO.Path]::GetFileNameWithoutExtension($rel)
    $ownModule = 'crate::' + ($ownParts -join '::')
    $refs = [System.Collections.Generic.List[string]]::new()
    foreach ($l in $code) {
        foreach ($r in (Get-Refs $l $baseSegs)) { $refs.Add($r) }
    }
    foreach ($ref in ($refs | Sort-Object -Unique)) {
        if ($ref -eq $ownModule -or $ref.StartsWith($ownModule + '::')) { continue }
        $ok = $false
        foreach ($a in $allowed) {
            if ($a -eq 'crate::' -or $ref -eq $a -or $ref.StartsWith($a + '::')) {
                $ok = $true
                break
            }
        }
        if (-not $ok) {
            Write-Host "VIOLATION  $rel : $ref  （允许：$($allowed -join ' / ') 或空）"
            $violations++
        }
    }
    $checked++
}

Write-Host "check_deps: 检查 $checked 个文件，违规 $violations 项（白名单基准：主规范第十一节 v9.17）"
if ($violations -gt 0) { exit 1 }
exit 0
