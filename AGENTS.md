# AGENTS.md — vxapo-driver

本文件面向在本仓库工作的**人**与 **AI**。核心只有一条：**格式不由人决定。**

---

## 1. 格式：`cargo fmt` 是唯一权威

- 风格配置在 `rustfmt.toml`（`style_edition = "2021"`、`newline_style = "Unix"`）。
  折行策略就是 rustfmt 默认值，**没有任何自定义 width 参数**——这是刻意的，
  目的是让任何编辑器 / CI / 工具开箱即产出同一格式。
- 工具链钉在 `rust-toolchain.toml`（`channel = "1.97.1"`）。**不要**在本地随意升级，
  见 §4。
- **不要手工微调格式**，也**不要**为局部观感引入新的格式化工具（prettier-rs、dprint 等）。
  觉得某处折行难看，先确认是不是真读不懂；真有问题用 §5 的定点豁免，并写明理由。

### 本仓库实测过的 rustfmt 行为（复审 diff 时不要误判为逻辑改动）

`cargo fmt` 除了调整空白，还会做这几类**等价**的机械改动：

1. **删除文件开头的 UTF-8 BOM**（U+FEFF）——本仓曾有 17 个文件带 BOM。
2. **补齐多行构造的尾随逗号**。
3. **重排 `use` 列表与 `mod` 声明**（按字典序）。
4. **给单表达式分支加/去花括号**（如 `=> expr` 展开成 `=> { expr }`）。
5. **给尾表达式补分号**（如 `else { return }` → `else { return; }`）。

---

## 2. 提交前：必须通过格式检查

```sh
cargo fmt --all -- --check     # 退出码 0、无输出才算通过
```

**新克隆的机器必须先执行一次**（hook 不随 clone 传播，这是它唯一的缺点）：

```sh
git config core.hooksPath .githooks
```

之后 `.githooks/pre-commit` 会在每次提交前自动跑检查，未格式化直接拒绝提交。
该 hook **只检查、不改写**——自动 fmt 会把未预期的改动混进提交、掩盖真实内容。

---

## 3. 纪律：fmt 提交必须纯净

**逻辑改动不得与整仓格式化放进同一提交。**

原因：`.git-blame-ignore-revs` 要靠「这个提交是纯格式化」这一事实才能安全跳过它。
一旦夹带逻辑改动，blame 会连真实改动一起忽略，等于毁掉可追溯性。

- 纯格式化提交：`style: cargo fmt --all（无逻辑改动）`
- 提交后把其**完整** hash 追加进 `.git-blame-ignore-revs`（并写一行注释说明）。
- 本机需执行一次 `git config blame.ignoreRevsFile .git-blame-ignore-revs`
  （GitHub 网页端会自动识别该文件）。

> 实测记录（2026-10）：格式化提交 `79b8729` 之后，全仓仅 27 行仍归属该提交，
> 且全部是 rustfmt **新造**的结构行（`}`, `},`, `);`, `assert!(` 等）。
> 纯缩进移动 git 自己就能正确 re-attribute，不需要 ignore 文件介入。

---

## 4. 升级 rustc 的流程（**独立事件**）

rustfmt 的输出随版本漂移。**不要**把升级和格式化混在一起做：

1. 先单独完成 rustc / 依赖升级并提交（格式可能暂时是红的）。
2. 升级后单独跑一次 `cargo fmt --all`，若**输出有变化**：
   - 单独成一个 `style:` 提交（同 §3 的纯净要求）；
   - 把该提交的完整 hash 追加到 `.git-blame-ignore-revs`；
   - 同步更新 `rust-toolchain.toml` 的 `channel`。
3. 若输出无变化：只更新 `rust-toolchain.toml` 即可。

---

## 5. 定点豁免（**尽量别用**）

个别宏展开 / 手工对齐表确实会被折得难读时，用 `#[rustfmt::skip]` **定点**豁免，
并在紧邻处写明理由。**大面积使用属于反模式**——它会重新制造不一致，
本计划的目标正是消灭不一致。

---

## 6. 行尾与编辑器

- 行尾由 `.gitattributes` 统一为 **LF**（`*.cmd` / `*.bat` 例外保持 CRLF）。
  这层覆盖全局 `core.autocrlf`，因此不会再有「内容没变却显示已修改」的假改动。
- `.editorconfig` 管非 Rust 文件的缩进/行尾。
- `.vscode/settings.json` 已入库，保存 Rust 文件即自动格式化。

---

## 7. 质量基线（改动前后都不得回退）

```sh
cargo test                                   # 481 passed; 0 failed; 1 ignored
cargo test --release                         # 471 passed; 0 failed; 1 ignored
cargo clippy --all-targets -- -D warnings    # 退出码 0
cargo fmt --all -- --check                   # 退出码 0
```

`1 ignored` 是真机用例
`install::audiodg::tests::active_dependents_enumerates_active_dependents`，
需显式 `cargo test -- --ignored` 才会跑（依赖真实机器状态）。

**clippy 这条线是从 222 条 warning 清到 0 的，不要回退。**
注意：`src/lib.rs` 里有 `#![deny(clippy::undocumented_unsafe_blocks)]`，
所以 **`unsafe` 块正上方必须紧贴 `// SAFETY:` 注释**。

> 踩过的坑（2026-10，本仓实证）：把单行 `if x { unsafe { .. } } else { .. }`
> 交给 rustfmt 展开成多行后，原本写在 `let` 上方的 `SAFETY` 注释与 `unsafe` 块之间
> 会多出一行 `if`，于是触发上述 deny。**SAFETY 注释必须放在 `unsafe` 块的正上方。**

---

## 8. 公开 API 只经 `lib.rs` 的 facade

**所有模块都是 `pub(crate)`**，对外只经 `src/lib.rs` 末尾的 facade（约 20 个 `pub use`）
暴露：`pub(crate) mod pipeline;` … + `pub use crate::pipeline::dsp::specs::{...}`。

**新增对外 API 必须同时在此登记**（`lib.rs` 顶部已注明），否则实现细节会随模块路径泄漏。

### 这直接决定了 `dead_code` 的语义（**容易误解，务必读**）

在这些 `pub(crate)` 模块里，**一个 `pub fn` 若无人调用，它就确实没被使用**——
既没有 crate 内消费者，也不构成对外 API（模块本身对外不可达）。

`dead_code` 判定只看**crate 内的可见性可达性**，与 `.def` 导出表或 `cdylib` **无关**
（最小实验实证）：

| `lib.rs` 写法 | 里面的 `pub fn` 无人调用时 |
|---|---|
| `pub(crate) mod inner;` | **报** dead_code（外面够不着 → `pub` 不构成豁免） |
| `pub mod inner;` | **不报**（模块对外可达 → 属公开 API，rustc 不能假设外部不用） |

**推论**：源码里的 `pub` 在 `pub(crate) mod` 之下只是「crate 内可见」的同义词。
**真正决定对外可见性的是 facade，不是 `pub` 关键字。**

---

## 9. `dead_code` 标注的处理约定

本仓有约 **45 处** `#[allow(dead_code)]`。它们**都带理由注释**，且经实证**诚实**
（剥离全部 allow 后 `cargo check` 仍为 0，报出的正是这些条目）。分四类：

| 类别 | 处理 |
|---|---|
| 「死簇：仅被已死的调用链引用」 | 需**逐簇整链评估**，属独立重构任务，勿顺手删 |
| 规范对表常量（`sys/consts.rs`、`sys/com/*` 的 `APOERR_*` / `*_SIGNATURE`） | **有意保留**，删了会削弱与 Windows SDK 对照能力 |
| 规范承诺的公开 API（`pipeline/realtime/contract.rs` 的 `RtSafe`/`RtCopy`/`rt_index*`） | **保留**（规范 4.7） |
| `#[cfg(test)]` 且测试都不用 | 可删（语义最明确） |

### 删死代码前必读的三条

1. **`never constructed` ≠ 无人使用。** 例如 `biquad.rs` 的 `BandPass`/`Notch`/`AllPass`
   有完整实现**且被测试构造并通过**，只是生产不调用。照 rustc 清单批量删会**连测试覆盖
   一起删掉**。
2. **同名陷阱会误删。** `MAX_FRAME_COUNT`（常量）vs `max_frame_count`（字段/方法）；
   `RegKey::value_exists`（方法，**有活调用**）vs 自由函数 `value_exists(root,..)`（已删）。
   **删前必须用 `\b` 全词匹配逐处确认。**
3. **判断「是否被用」只能靠 rustc，不能靠 grep。** `.method(` 形式会严重误判——
   实测 `initialize` 全仓 102 处「调用点」，但 `ChildApo::initialize` 零调用。

**另注**：`#[cfg(test)]` 的条目在 `cargo check --lib` 下**不报**（根本不编译），
必须用 `--all-targets` 才看得见全貌。

---

## 10. RT-safety 契约已接入生产路径（规范 4.5）

`pipeline/realtime/contract.rs` 的契约**已在生产路径生效**（此前是未接线的死设施）：

| 侧 | 位置 | 机制 |
|---|---|---|
| RT 入口 | `object/apo/process.rs::apo_process` | 创建 `RtGuard` 建立 RT 上下文 |
| RT 核心 | `apo_process_inner` | `rt_assert_in_rt!` — 只允许经 RT 入口调用 |
| 非 RT | `object/apo/reload.rs::hot_reload_impl`（取锁 / 文件 I/O） | `rt_require_non_rt!` |

release 下全部编译为空操作（零开销）。**改动这两条路径时注意**：若在 RT 路径上引入
取锁、分配或 I/O，debug 构建会**当场 panic**——这是设计意图，不是 bug。

### 两个已修的历史缺陷（勿回退）

原实现是**全局 `AtomicBool` + 无计数**，接入后实测暴露两个真缺陷，现已改为
**thread-local 深度计数**：

1. **嵌套守卫提前清除上下文**：内层 `RtGuard` Drop 会连带清掉外层仍有效的上下文。
2. **上下文跨线程泄漏**：全局标志会被并发的 watcher 线程读到 → `rt_require_non_rt!`
   抛**假**违例。

回归测试：`rt_context_is_thread_local`、`rt_depth_does_not_underflow`，以及
`rt_guard_nested` 中补上的「内层 Drop 后外层仍应有效」断言。

---

## 11. 测试全局状态的串行化

`INST_COUNT`（`object/ref_count.rs`）与 `LOCK_COUNT`（`object/factory.rs`）是**进程级
全局量**，被 `object/dll_exports.rs` 与 `object/factory.rs` **两个测试模块**的用例读写，
各自先 `reset_for_test()` 再断言精确值。

**锁必须放在共享位置**：`crate::object::ref_count::serial_lock()`。
任何读写这两个计数的测试，第一条语句取此锁。

> 曾经的错误做法：在两个测试模块里**各放一把锁**——跨模块竞态依然存在，实测 release 下
> 仍会失败。锁必须共享。
