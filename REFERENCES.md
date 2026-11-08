# XL-OS 参考资料清单

> 状态：草案 v0.1 · 2026-11-08
> 用法：这份清单按**用途**组织，不按星数。每一条都标了「读什么」和「什么时候读」，因为读错的时机和读错的项目一样浪费时间。
> 许可标注以各仓库 LICENSE 为准；标「待确认」的表示本文未核实到确定结论。

---

## 0. 使用原则

1. **M0 之前只读两个东西**：xv6 源码 + blog_os。别的都是干扰。
2. **架构看 Managarm，工程看 SerenityOS，入门看 xv6。** 三个项目分工明确，不要混着读。
3. **许可证决定读法**：宽松许可 = 可以读实现细节；GPL 系 = 只读架构和语义，不读实现。
4. **不要一开始读 Linux 源码。** 那是写完之后查细节用的，不是设计期参考资料。

---

## 1. 架构直接对标（最重要）

### Managarm — 本项目的第一参考

- 仓库：https://github.com/avdgrinten/managarm
- 许可：MIT 系（以仓库 LICENSE 为准）
- 架构：完全异步微内核（thor）+ 用户态 Linux ABI 兼容（helios）
- **读什么**：设计文档、IPC/异步模型、syscall 覆盖面策略、内存管理
- **什么时候读**：M1 开始读设计文档，M4 之前通读完 IPC 部分
- **为什么是第一**：全清单里唯一「微内核 + Linux ABI 兼容且跑起真实程序」的项目，架构和本项目同形

### Starnix（Fuchsia 内部组件）

- 位置：Fuchsia 源码树内，非独立仓库
- 许可：Fuchsia 项目许可（MIT/BSD/Apache 混合）
- **读什么**：syscall 覆盖策略、哪些先返回 ENOSYS 的取舍、兼容性回归测试套件、issue tracker 里的真实坑
- **什么时候读**：M9 之前
- **注意**：它的价值在策略不在代码

### gVisor

- 仓库：https://github.com/google/gvisor
- 许可：Apache-2.0
- 架构：用户态内核，拦截 syscall 自行实现
- **读什么**：Systrap 平台（如何低成本拦截 syscall）
- **什么时候读**：M9 之前，如果决定先做薄兼容层
- 说明：https://gvisor.dev/docs/architecture_guide/platforms/

### Redox

- 仓库：https://github.com/redox-os/redox
- 许可：MIT
- 架构：Rust 微内核 + 自研 libc（relibc）+ 从零用户态
- **读什么**：许可策略、自研 libc 的取舍、Rust 写内核的实际工程问题
- **什么时候读**：M0–M1（对齐 D001 的 Rust 决策，看别人踩过的坑）

---

## 2. 中文同行（要看，但看的是现实不是理想）

| 项目 | 许可 | 看什么 |
|---|---|---|
| [vivo BlueKernel](https://atomgit.com/vivoblueos/kernel) | 待确认 | 国内工业级自研 Rust 内核的工程化方式 |
| [DragonOS](https://github.com/dragonos-community/dragonos) | 待确认 | 最接近本项目目标的社区项目，观察真实推进节奏 |
| [haoos](https://github.com/haoxunwl/haoos) | 自定义（可商用、二开可闭源需声明） | **反面教材**：翻 commit 曲线看「三架构并行」的真实代价 |

**关键观察：这些项目全部是「自研内核」，没有一个做中文界面层 / 中文命令 / 中文编程语言。**
→ 中文层是空白：没有前人可参考，也没有人抢；同时意味着这块没有现成经验可用。

---

## 3. 入门必读（M0–M3）

| 材料 | 许可 | 读什么 | 时机 |
|---|---|---|---|
| [xv6-riscv](https://github.com/mit-pdos/xv6-riscv) | MIT | **完整通读一遍**，约 8000 行，建立「内核是什么形状」的直觉 | M0 之前，一周内 |
| [blog_os](https://os.phil-opp.com/) | MIT | Rust 内核教程，配套 crate 生态 | M0–M3 |
| [OSDev Wiki](https://wiki.osdev.org/) | CC | 查具体问题（尤其「为什么 triple fault」） | 全程 |
| [SerenityOS](https://github.com/SerenityOS/serenity) | BSD-2-Clause | **读它的开发纪律**：怎么保证主干永远可启动 | M1 之后 |

**注意**：读 SerenityOS 不是读架构（它是宏内核），是读「一个人怎么把系统做到能日常用」的工程方法。

---

## 4. 可直接使用的依赖（不是参考，是依赖）

| 用途 | 项目 | 许可 | 备注 |
|---|---|---|---|
| 引导器 | [limine](https://github.com/limine-bootloader/limine) | BSD-2-Clause | x86_64 / aarch64，省掉自研 UEFI loader |
| Rust 内核基础 | [rust-osdev](https://github.com/rust-osdev) | MIT/Apache-2.0 | x86_64 / bootloader / uart_16550 等 crate |
| 编译器后端 | LLVM | Apache-2.0（含 LLVM 例外） | 自研语言前端接它 |
| 可移植分发 | WebAssembly | Apache-2.0 | 跨架构分发格式 |
| GPU 驱动 | Mesa | MIT | 用户态驱动的核心参考与组件 |
| Wayland 合成器 | wlroots | MIT | 图形栈 M6 参考 |
| 本地模型 | llama.cpp | MIT | `sys-nl` 本地推理 |
| 简繁转换 | OpenCC | Apache-2.0 | 中简体/中繁体互转 |
| 中文分词 | jieba | MIT | 中文命令与自然语言输入 |
| 图形 | tiny-skia / LVGL | MIT | UI 与嵌入式界面 |
| Binder | AOSP libbinder | Apache-2.0 | M4 IPC 设计的头号工程参考 |

---

## 5. 许可红线（MIT 项目必须守住）

### 只能读架构和语义，不能读实现，不能链接

| 项目 | 许可 |
|---|---|
| Linux 内核 | GPL-2.0 |
| QEMU | GPL-2.0（作为开发工具使用没问题，不可链接进产品） |
| ReactOS | GPL-2.0 |
| Waydroid | GPL-3.0 |
| WireGuard | GPL-2.0（只能进程隔离集成） |
| Wine | LGPL-2.1（动态链接可讨论，静态链接不行） |
| Syncthing | MPL-2.0（文件级 copyleft，进程隔离集成） |

### 宽松许可，可参考甚至可复用

seL4（BSD-2）、Managarm、Redox（MIT）、SerenityOS（BSD-2）、xv6（MIT）、Theseus（MIT）、gVisor（Apache-2.0）、Box64 / FEX-Emu（MIT，ARM 上跑 x86 二进制用）、libbinder（Apache-2.0）。

### 绝对不能碰

2020 年泄漏的 Windows XP / Server 2003 源码。**不是开源，是泄漏。** 看过就会污染项目的 clean-room 地位。
合法替代：《Windows Internals》（书）、Helen Custer《Inside Windows NT》（书）、ReactOS 源码（GPL，只读）。

---

## 6. 阅读路线（按里程碑）

| 阶段 | 读什么 | 目的 |
|---|---|---|
| M0 之前（一周） | xv6 通读 + blog_os 前 5 章 | 建立直觉 |
| M1–M3 | Managarm 设计文档 + OSDev Wiki 查缺 | 定 IPC 与内存管理的形状 |
| M4 之前 | Managarm IPC/异步设计 + AOSP Binder | **IPC 是生死线**，必须读透 |
| M5–M8 | SerenityOS 的工程纪律 + LWN 子系统设计文章 | 保持可启动、理解设计取舍 |
| M9 之前 | Starnix 策略 + gVisor Systrap + WoW64/ntdll | 兼容层架构 |

---

## 7. 永远不要做的事

- 一开始读 Linux 源码「参考实现」
- 同时读两个以上内核的源码
- 读 GPL 项目的实现细节然后写进 MIT 项目
- 碰泄漏的 Windows 源码
- 用 haoos 的三架构描述当可行性论据
