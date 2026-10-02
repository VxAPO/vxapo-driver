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
cargo test                                   # 491 passed; 0 failed; 1 ignored
cargo test --release                         # 483 passed; 0 failed; 1 ignored
cargo clippy --all-targets -- -D warnings    # 退出码 0
cargo fmt --all -- --check                   # 退出码 0
```

`1 ignored` 是真机用例 `active_dependents_enumerates_active_dependents`，
需显式 `cargo test -- --ignored` 才会跑（依赖真实机器状态）。

**clippy 这条线是从 222 条 warning 清到 0 的，不要回退。**
注意：`src/lib.rs` 里有 `#![deny(clippy::undocumented_unsafe_blocks)]`，
所以 **`unsafe` 块正上方必须紧贴 `// SAFETY:` 注释**。

> 踩过的坑（2026-10，本仓实证）：把单行 `if x { unsafe { .. } } else { .. }`
> 交给 rustfmt 展开成多行后，原本写在 `let` 上方的 `SAFETY` 注释与 `unsafe` 块之间
> 会多出一行 `if`，于是触发上述 deny。**SAFETY 注释必须放在 `unsafe` 块的正上方。**
