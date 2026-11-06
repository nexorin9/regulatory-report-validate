# 院内离线安装与运维说明

> 给信息科：这份文档讲的是**在一台没有外网的专机上，把引擎和表格插件装好并跑出第一次可核对的校验结论**。
> 全程不需要连互联网，也不需要装数据库服务。

- 引擎是一个单文件二进制（`regval`），自带 SQLite，状态存在本机一个文件里；
- 表格插件是三个 VBA 模块（Excel）或一个 JSA 文件（WPS），跑在科室自己的上报表里；
- 校验环节**完全离线**：不联网也能把必填缺失、枚举越界、单位不符、疑似重复四类问题算出来；
  只有「上报」与「查科室主数据」两件事需要连院内系统。

---

## 0. 交付物清单

装之前先对一遍，缺哪件后面装不上。

| 交付物 | 是什么 | 装到哪 | 必需 |
|--------|--------|--------|------|
| `regval`（Windows 上是 `regval.exe`） | 引擎单二进制，含 SQLite、读表、规则引擎、重复检测、幂等上报 | `C:\Program Files\RegVal\regval.exe` | 是 |
| `rules/` | 版本化口径规则表 + 27 项指标口径种子 + 重复检测阈值 | `C:\Program Files\RegVal\rules\` | 是 |
| `macro/vba/RegVal.bas` | Excel 加载宏：「校验」「上报」两个按钮 + 状态条 + 5 个新列写回 | 导入进上报表工作簿（或 `.xlsm` 模板） | Excel 入口必需 |
| `macro/vba/RegValTokens.bas` | 样式 token 常量模块（**由引擎生成**，不要手改） | 同上，第二个模块 | Excel 入口必需 |
| `macro/vba/RegValMessages.bas` | 提示文案与错误码中文说明常量模块（**由引擎生成**） | 同上，第三个模块 | Excel 入口必需 |
| `macro/wps/regval.js` | WPS JSA 版宏（单文件） | 院内 WPS 报送机上加载 | WPS 入口必需 |
| `.env.example` | 配置模板（**不含任何凭据**） | 复制成 `.env` 后按院内地址填写 | 是 |
| `samples/dataset/` | 脱敏合成样例：6 张科室表 + 合并表（1200 行） | 留着跑最小验证 | 建议 |
| `docs/install.md` | 本文件 | 信息科留档 | 是 |
| `vendor/` + `src/` + `Cargo.lock` | 源码与全部依赖（**仅当专机上要现场编译时需要**） | 见第 1 节 | 可选 |

> **两个「二进制 + 宏」的交付形态**：信息科拿到的是**编译好的引擎 + 宏**，
> 科室不需要装 Rust 工具链，也不需要碰源码。只有当院内要求「在自己机器上从源码重建、
> 审计每一行依赖」时，才需要第 1 节的离线构建流程。

---

## 1. 离线构建（无外网专机）

### 1.1 先决定走哪条路

| 路线 | 谁做 | 需要什么 | 什么时候选它 |
|------|------|----------|--------------|
| **A. 直接拿二进制**（推荐） | 信息科 | 一台装了 Excel/WPS 的专机 | 绝大多数医院 —— 引擎不依赖 Rust，拷过去就能跑 |
| **B. 专机上现场编译** | 信息科/厂商工程师 | Rust 工具链 + 依赖目录 `vendor/` | 院内要求「从源码重建 + 可审计依赖」，或要在本机改口径规则表 |

路线 B 的完整步骤在 1.2–1.5。

### 1.2 联网机：把依赖生成成本地目录

在**能上网**的机器上（开发机、CI、厂商自己的构建机），进项目根目录：

```bash
cd regulatory-report-validate
export PATH="$HOME/.cargo/bin:$PATH"      # cargo 常常不在默认 PATH 里
./scripts/offline-build.sh --vendor
```

这一步做两件事：

1. `cargo vendor --versioned-dirs vendor/` —— 把 `Cargo.lock` 里锁定的**全部**依赖
   （本项目约 210 个 crate、250 MB 上下，含 Windows/Linux/macOS 三套平台依赖）
   的源码复制到 `vendor/`；
2. 把「从哪里取依赖」的切换写进 `.cargo/config.toml`：

```toml
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor/"
```

> **`.cargo/config.toml` 不要提交进仓库**。依赖目录不进库（250 MB），
> 仓库里留一份指向不存在目录的配置，会让所有人 clone 下来就编不动。
> 它由 `scripts/offline-build.sh` 在需要时生成。

手工做等价的两条命令（脚本就是这么干的）：

```bash
cargo vendor --versioned-dirs vendor/ > .cargo/config.toml
```

### 1.3 专机：拷贝 + 离线构建

把**整个项目目录**（含 `vendor/`、`src/`、`Cargo.toml`、`Cargo.lock`）拷到专机，然后：

```bash
cd regulatory-report-validate
export PATH="$HOME/.cargo/bin:$PATH"
./scripts/offline-build.sh
```

脚本依次做三件事：

1. **校验依赖目录完整性** —— 每个 crate 目录里必须有 `.cargo-checksum.json`
   （这正是 cargo 自己校验 vendored source 用的文件），缺一个就报「依赖目录不完整 —— 拷贝时断过」；
2. **写 `.cargo/config.toml`**，把依赖来源切到 `vendor/`；
3. **执行 `cargo build --release --offline`** —— `--offline` 保证这一步**一个字节都不往外发**。

产物在 `target/release/regval`（Windows 上是 `regval.exe`）。

### 1.4 依赖目录完整性校验（可单独跑）

```bash
./scripts/offline-build.sh --check
```

真实输出：

```text
依赖目录：vendor/ 共 210 个 crate，校验文件齐全 210 个，不完整 0 个
Cargo.lock 声明 211 个包（含本项目自身 1 个），依赖目录 210 个 crate
```

> **数字对不上就重拷**：`Cargo.lock` 里的包数应当比依赖目录的 crate 数**多 1**
> （多出来的那 1 个是本项目自身，它不进依赖目录）。

### 1.5 Windows 专机

院内专机基本都是 Windows x64。两种做法：

**做法一（推荐）：在 Windows 或交叉编译机上直接出 `regval.exe`**

```bash
# Linux/macOS 上交叉编译 Windows 版
rustup target add x86_64-pc-windows-msvc
cargo build --release --offline --target x86_64-pc-windows-msvc
# 产物：target/x86_64-pc-windows-msvc/release/regval.exe
```

> 注意：`rusqlite` 用的是 bundled SQLite，交叉编译时需要一个 Windows 版 C 编译工具链；
> 装不上就改在 Windows 机器上直接构建（引擎本身编译很快，依赖编完约 2–3 分钟）。

**做法二：在 Windows 专机上装 Rust 工具链后本地编译**

1. 装 Rust（`rustup-init.exe`，院内镜像或离线 U 盘均可）；
2. `set PATH=%USERPROFILE%\.cargo\bin;%PATH%`；
3. 在项目目录执行 `scripts\offline-build.sh`（Windows 上的 Git Bash / WSL 均可；纯 cmd 下把脚本里的命令逐条敲一遍）。

### 1.6 打成一份可拷贝的安装包

```bash
./scripts/package-offline.sh                    # 运行时包：二进制 + 宏 + 规则 + 样例 + 本文档
./scripts/package-offline.sh --with-source      # 再加源码与依赖目录（专机现场编译用）
```

产物在 `dist/regval-offline-<日期>-<平台>.tar.gz`，同目录有 `.sha256`。
包内**不含任何凭据**（`.env` 不进包，只带 `.env.example`）。
拷到专机后先核对：

```bash
sha256sum -c SHA256SUMS        # 逐文件比对，对不上就是拷贝时断过
```

---

## 2. 院内安装说明

### 2.1 目录布局

按这个布局放，后面所有路径都照抄：

```text
C:\Program Files\RegVal\
├── regval.exe                    # 引擎二进制（只读可执行）
├── rules\                        # 口径规则目录（只读）
│   ├── rules-2021.1.rules.json   # 版本化规则表（11 条口径规则）
│   ├── indicators-2021.1.seed.json  # 27 项指标口径种子
│   ├── dupe-distance.json        # 重复检测第 3 级阈值
│   └── roster.json               # 应交科室名册（**由医院自己维护**，产品不自带）
├── macro\
│   ├── vba\RegVal.bas
│   ├── vba\RegValTokens.bas
│   ├── vba\RegValMessages.bas
│   └── wps\regval.js
└── state\                        # 状态库所在目录（要可写）
    └── state.db                  # 引擎首次运行自动建
```

> **`roster.json`（应交科室名册）由医院自己维护，产品不自带**。
> 「未上报名单与催报对照」要靠它算出「哪些科室该交还没交」；
> 没有它时 `regval export pending` 仍能导出已交与未交的科室清单，
> 但**「未收到」那一列会留空并标「名册来源：未取到名册」** ——
> 不会拿本地见过的科室凑一份名册出来。
> 规则表与口径种子是产品的一部分（随交付走），名册是院内的事实（每次报告期由医院更新）。

> **状态库不要放在 `Program Files` 目录树以外的只读位置**。
> `C:\Program Files\RegVal\state\` 需要经办人账号**可写**；
> 如果院内的权限策略不让普通用户写 `Program Files`，
> 把状态库放到本地磁盘（例如 `D:\regval-state\state.db`）并改 `REGVAL_STATE_DB`。

### 2.2 环境变量注入（三种方式，按院内规范挑一种）

引擎的**所有**配置都从环境变量读，程序内不硬编码、不落盘。凭据尤其不能写进代码或表格。

| 方式 | 怎么做 | 适合 |
|------|--------|------|
| **A. `.env` 文件（最省事）** | 把 `.env.example` 复制成 `.env`，填好实际值，放在**引擎的工作目录**里 | 单机单用户；开发机 |
| **B. 系统环境变量** | 「此电脑 → 属性 → 高级系统设置 → 环境变量」，逐项新增 | 多用户共享同一台机器 |
| **C. 计划任务 / 启动脚本** | 在任务计划程序里给任务加环境变量，或用 `.bat` 先 `set` 再调 | 定时跑批；要给不同账号不同的凭据 |

> **优先级**：进程环境 > `.env` 文件。用计划任务注入的环境变量**不会被 `.env` 覆盖**，
> 这正是「系统级配一次、`.env` 只留本机开发值」想要的语义。

`.env` 方式：

```bash
cp .env.example .env
# 填 REGULATORY_REPORT_BASE_URL / REGULATORY_REPORT_TOKEN / REGVAL_STATE_DB / REGVAL_RULE_DIR
```

计划任务方式（Windows 批处理）：

```bat
@echo off
set "REGULATORY_REPORT_BASE_URL=https://上报网关院内地址"
set "REGULATORY_REPORT_TOKEN=＜院内发放的 Bearer 凭据＞"
set "REGVAL_STATE_DB=D:\regval-state\state.db"
set "REGVAL_RULE_DIR=C:\Program Files\RegVal\rules"
"C:\Program Files\RegVal\regval.exe" validate --input "%~1" --out "%~2"
```

**必填项与用途**（完整清单见 README「命令 / API / 配置说明 → 关键配置项」）：

| 变量 | 谁需要它 | 不配会怎样 |
|------|----------|------------|
| `REGVAL_STATE_DB` | validate / status / push / merge / export / replay | 直接以 `CONFIG_MISSING` 退出，点名缺这一项 —— **不会**默默另开一个库 |
| `REGVAL_RULE_DIR` | rules | 退回内置规则集（口径结论相同，但换版确认与差异清单用不了） |
| `REGULATORY_REPORT_BASE_URL` + `REGULATORY_REPORT_TOKEN` | push | 上报时以 `CONFIG_MISSING` 退出；**校验环节不受影响** |
| `HIS_BASE_URL` + `HIS_API_TOKEN` | validate / push / dept | 不配也能跑 —— 结果里标「科室主数据未校验」（这一轮没查，不静默通过） |
| `OPENAI_BASE_URL` / `OPENAI_API_KEY` / `OPENAI_MODEL` / `OPENAI_TIMEOUT_SECS` | validate / merge | 语义归并这一级留空，其余三级照跑；只标「疑似」，不合并不推送 |

> **还没拿到院内接口的，把 `.env` 里那几行占位地址删掉**
> （`https://his.internal.example`、`https://regulatory-gateway.internal.example` 都是占位值）。
> 留着会让每一轮都去连一个不存在的地址，结果里写成「目录拉取失败」。

### 2.3 权限清单

| 位置 | 需要什么权限 | 说明 |
|------|--------------|------|
| 引擎二进制目录 | 读 + 执行 | 普通用户可读即可 |
| 规则目录 `rules/` | 读 | 换口径版本时由质控岗改，**不要**让经办人随手改 |
| 状态库目录（`REGVAL_STATE_DB` 的父目录） | 读 + **写** | 经办人账号要能建文件 |
| 上报表所在目录 | 读 + **写** | 宏在同目录留一份结果契约与错误报告 JSON，校验后可手工删除 |
| 宏所在的工作簿 | 无需写权限 | 宏只写新列那几格 |

**院内机常见坑**：域策略把 `%TEMP%` 或用户目录设成只读 → 状态库写不进去。
把 `REGVAL_STATE_DB` 指到本地磁盘（如 `D:\regval-state\state.db`）比指到用户目录稳。

### 2.4 首次安装自检

装完先跑一次自检，**逐项**给结论，不只输出「OK」：

```bash
regval doctor
```

真实输出（配置齐备时）：

```text
regval doctor · 运行环境与凭据逐项自检 · 2026-11-07 06:06:39
[提示] 工具链    cargo — PATH 里没有 cargo，但 /Users/kamini/.cargo/bin 存在
       ↳ 补法：构建前执行：export PATH="$HOME/.cargo/bin:$PATH"（本机目录 /Users/kamini/.cargo/bin）
[正常] 状态库    ./.regval/state.db — 状态库已存在且可写
[正常] 规则目录   ./rules — 规则目录存在（4 个条目）
[正常] 配置项    REGULATORY_REPORT_BASE_URL — 已设置：http://127.0.0.1:8731
[正常] 配置项    REGULATORY_REPORT_TOKEN — 已设置：（已设置，长度 10）
...
结论：12 项正常 · 1 项提示 · 0 项缺失（无缺失项，可开始校验）
```

退出码：**0 = 无缺失项，可以开始校验**；**1 = 有缺失项，先按每项下方的补法补齐**。

---

## 3. 宏导入与权限说明

### 3.1 Excel

1. 打开 Excel → `文件 → 选项 → 信任中心 → 信任中心设置 → 宏设置`；
2. 选择「**信任对 VBA 工程对象模型的访问**」或把上报表所在目录设为「**受信任位置**」
   （院内网环境通常由信息科统一加白名单）；
3. `开发工具 → 插入 → 模块`（**建三个模块**），依次粘贴：
   - 第一个模块 ← `macro/vba/RegVal.bas`
   - 第二个模块 ← `macro/vba/RegValTokens.bas`
   - 第三个模块 ← `macro/vba/RegValMessages.bas`
4. `Alt+F8` 运行 **`RegVal_InstallBar`** —— 表右侧出现「校验」「上报」两个按钮，
   数据下面出现状态条。（按钮可以不装：直接运行 `RegVal_Validate` 时宏会顺手补上。）

> **三个模块都要导入**。少导一个会编译报「变量未定义」。
>
> **粘贴时若报「无效的属性」**：模块文件第一行的 `Attribute VB_Name = "..."` 只在 `.bas`
> 文件里合法，粘进 VBE 报错时删掉那一行即可，不影响功能。
>
> **编码**：模块文件是 UTF-8，中文 Windows 的 VBE 按 ANSI（GBK）读模块文件，直接导入可能乱码。
> 导入前用记事本「另存为 → 编码 → ANSI」另存一次即可。宏读契约 JSON 时自己按 UTF-8 解码，
> 所以**只要模块本身的中文没乱码，写回原表的中文就是对的**。

### 3.2 WPS

WPS 侧不是「Excel 宏换个扩展名」—— JSA 的对象模型与 VBA 不同，所以另有一份宏
`macro/wps/regval.js`（**由引擎从模板装配生成，不要手改**）。

1. 确认 `macro/wps/regval.js` 是最新装配出来的（改过模板后跑 `cargo run -- macro --out macro/wps/regval.js`）；
2. 拷到院内 WPS 报送机（**一个文件即可**，不需要别的模块）；
3. WPS 表格里打开上报表 → `开发工具 → JSA 宏 / 加载宏` → 选 `regval.js`；
4. 运行 `RegVal_InstallBar`。

### 3.3 版本对齐（引擎 ↔ 宏 ↔ 契约）

三方必须来自**同一次交付**：

| 部件 | 版本落在哪 | 不一致时的表现 |
|------|------------|----------------|
| 引擎 ↔ 表格插件 | 契约 `schema_version`（当前 `regval.contract/1.0.0`） | 宏侧版本闸门拦截：**提示「引擎与宏版本不一致，请升级」并中止写回，不做部分写回**；引擎侧报 `CONTRACT_VERSION_MISMATCH` |
| 引擎 ↔ 表格插件 | 5 个新列的列名数组 | 列名闸门拦截，同样中止写回 |
| 引擎 ↔ 口径规则 | 规则表版本号（当前 `2021.1`）+ 指纹 | 换版未确认时继续用旧版并提示（`RULE_SWITCH_UNCONFIRMED`） |

**怎么核对当前版本**：

```bash
# 引擎侧的契约版本
regval validate --input <样例表> --out result.json
python3 -c "import json;print(json.load(open('result.json'))['schema_version'])"
# → regval.contract/1.0.0

# 宏侧的版本常量（Excel）
grep REGVAL_SCHEMA_VERSION macro/vba/RegVal.bas
# → Public Const REGVAL_SCHEMA_VERSION As String = "regval.contract/1.0.0"

# 主动验闸门：引擎对不上就拒绝出契约
regval validate --input <样例表> --out result.json --expect-schema-version regval.contract/9.9.9
# → 退出码非 0，stderr 含 CONTRACT_VERSION_MISMATCH，且**不产出**契约文件
```

**换版流程**（口径变了，不是宏变了）：

```bash
regval rules --dir ./rules                    # 看当前生效版本与清单
regval rules approve --version 2022.1 --approver 质控岗张老师   # 质控岗确认
regval export diff --from-version 2021.1 --to-version 2022.1 --out 差异.csv
```

未确认就换版时，引擎**继续用旧版**并明确提示，不会让历史批次的结论悄悄漂移。

> `export diff` 的两个版本号必须**都在规则目录里**——它比的是两份规则表，
> 新版还没放进目录时报 `RULE_VERSION_NOT_FOUND`（缺 `--to-version` 时默认取目录里版本最高的那一版）。

### 3.4 改部署路径（必做）

宏顶部两行常量要按院内实际路径改（Excel 与 WPS 各改各的）：

```vba
' Excel：macro/vba/RegVal.bas
Public Const REGVAL_ENGINE_PATH As String = "C:\Program Files\RegVal\regval.exe"
Public Const REGVAL_STATE_DB As String = "C:\Program Files\RegVal\state\state.db"
```

```javascript
// WPS：macro/wps/regval.js
var REGVAL_ENGINE_PATH = "C:\\Program Files\\RegVal\\regval.exe";
var REGVAL_STATE_DB = "C:\\Program Files\\RegVal\\state\\state.db";
```

命令行里所有路径都用 `Chr$(34)` 包引号，所以**路径里有空格或中文都不会散架**
（如 `D:\院内工具\上报校验\regval.exe`）。

> WPS 宏还支持用环境变量 `REGVAL_ENGINE_PATH` 覆盖宏里的常量 —— 换机器时改环境变量即可，
> 不用重新装配宏。

### 3.5 卸载与回滚

| 要做的事 | 怎么做 |
|----------|--------|
| 撤掉表格插件 | 删掉工作簿里导入的三个模块（或换回未导入的 `.xlsx`） |
| 撤掉引擎 | 删 `C:\Program Files\RegVal\` 目录 |
| 清状态库 | 删 `state.db`（**先备份**，见 4.4） |
| 换引擎版本 | 换二进制 + 换宏（**必须同一次交付**），换完先跑 `regval doctor` |

---

## 4. 运维清单

### 4.1 `regval doctor` 逐项解读

自检分四类共 13 项，**逐项**给结论：

| 分类 | 检查什么 | 「正常」长什么样 | 「缺失」的处理动作 |
|------|----------|------------------|------------------|
| 工具链 | `cargo` 在不在 PATH | `cargo 已就绪：/usr/local/bin/cargo` | 装 Rust 工具链后把 `~/.cargo/bin` 加入 PATH |
| 状态库 | 路径在哪儿、能不能写 | `状态库已存在且可写` / `所在目录可写（…），首次运行会新建状态库` | 换一个可写目录，或修正目录权限 |
| 规则目录 | 目录在不在、是不是目录 | `规则目录存在（4 个条目）` | 创建该目录，或修正 `REGVAL_RULE_DIR` |
| 配置项 | 10 个环境变量逐个查 | `已设置：（已设置，长度 10）`（敏感项只给长度，不回显内容） | 在 `.env` 里填写该变量 |

三种结论的读法：

- **`[正常]`** —— 这项可用；
- **`[提示]`** —— 能用，但有隐患（例如「PATH 里没有 cargo」—— 已经装好的引擎不受影响，只是这台机器上没法现场编译）；
- **`[缺失]`** —— 这项不可用，**每一项下面都直接给补法**，照着做即可。

> 「提示」不影响退出码。只有出现 `[缺失]` 时 `regval doctor` 才以退出码 **1** 结束。

### 4.2 常见错误码与处理动作

错误码是机器可读的，中文说明与处理动作的**唯一出处**是引擎的 `src/errors.rs`；
表格插件弹窗里显示的话就是这里的内容。下面按场景分组。

**输入与表结构**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `INPUT_UNREADABLE` | 输入表读取失败 | 确认文件路径与读取权限；Excel 另存为 `.xlsx` 后重试 |
| `UNSUPPORTED_FORMAT` | 不支持的输入格式 | 本轮支持 `.csv` / `.xlsx` / `.xlsm`，先把表另存为 xlsx |
| `MISSING_COLUMN` | 表头缺少必填列 | 检查表头是否写错，或多表并入时列被挤掉 |
| `EMPTY_TABLE` | 表中没有数据行 | 确认表头之外至少有一行数据 |

**口径规则**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `RULE_SYNTAX_ERROR` | 口径规则语法不通过 | 修正规则 SQL 后重新装载；**该规则在此之前不生效**（不静默通过） |
| `RULE_LOAD_FAILED` | 口径规则表装载失败 | 按报错里的文件名与字段名修正规则表；没装载上的规则不会参与校验 |
| `RULE_BASIS_MISSING` | 口径规则缺官方依据 | 给该条规则补上官方依据出处（文件与条款），补齐后再装载 |
| `RULE_SWITCH_UNCONFIRMED` | 口径换版未经质控岗确认 | 请质控岗确认后执行 `regval rules approve`；**未确认前本轮继续用旧版** |
| `RULE_EFFECTIVE_DATE_NOT_REACHED` | 口径版本的生效日期未到 | 按规则表里的 `effective_from` 确认启用日期 |
| `RULE_VERSION_NOT_FOUND` | 规则集里没有指定版本 | 确认规则目录里有该版本的口径表，或改用已装载的版本 |
| `DENOMINATOR_ZERO` | 分母为 0 | 先核对该行分母口径，补齐后再填分子与填报值（**与「公式不符」分开归类**） |
| `INVALID_NUMERIC` | 数值列不是合法数值 | 把该格改成数值；**非数值与空值是两类不同问题** |

**配置与凭据**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `CONFIG_MISSING` | 缺少必需的环境变量 | 复制 `.env.example` 为 `.env` 并填齐；凭据不要写进代码 |
| `CONFIG_INVALID` | 环境变量取值不合法 | 按该项的取值格式修正后重跑 |

**契约与写回**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `CONTRACT_VERSION_MISMATCH` | 引擎与宏版本不一致，请升级 | 引擎与宏换用**同一次交付**里的版本；不做部分写回 |
| `CONTRACT_WRITE` | 结果契约写出失败 | 确认输出目录可写且磁盘有空间 |

**上报与状态闭环**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `UNCONFIRMED_ROW` | 该行未经人工确认，不能上报 | 在原表勾选确认后再上报；**未确认行一律不推** |
| `CONFIRM_ACTOR_MISSING` | 人工确认缺确认人 | 确认时写明确认人（`regval confirm --by 姓名`）；**空名不入库** |
| `IDEMPOTENCY_MISSING` | 缺幂等键 | 重跑上报计划编译；幂等键只由引擎生成，不接受外部传入 |
| `INVALID_STATE_TRANSITION` | 行级状态迁移不合法 | 按报错里的两个状态看这一行该从哪来；已上报的行不会被普通校验改回 |
| `ROW_STATE_CONFLICT` | 行状态版本冲突 | 这一行刚被另一个动作推进过；重跑当前动作即可，**不要手工改状态** |
| `GATEWAY_REJECTED` | 上报网关退回 | 按回执里的错误码与字段名修正后重新上报（4xx **不重试**） |
| `GATEWAY_SERVER_ERROR` | 上报网关返回服务端错误 | 退避重试；重试耗尽转「待重试」，**不改成已上报** |
| `GATEWAY_TIMEOUT` | 上报网关调用超时 | 调大 `REGULATORY_REPORT_TIMEOUT_SECS` 或排查院内网段；**超时不算「已上报」** |
| `GATEWAY_CONNECTION_FAILED` | 上报网关连不上 | 确认网关地址可达；本行转「待重试」，**保留幂等键** |
| `GATEWAY_RESPONSE_INVALID` | 上报网关回执结构不合法 | 该行标「回执待人工核对」；**不要按已上报处理** |
| `RECEIPT_CONFLICT` | 幂等键与回执 ID 不是一一对应 | 同一幂等键只允许一条回执、同一回执号只允许挂一个幂等键；查清重复回执来源再重跑 |
| `RECEIPT_LEDGER_MISMATCH` | 回执台账交叉核对不通过 | 按台账里列出的冲突逐条查（幂等键 ↔ 回执号 ↔ 行状态）；**查清前这份台账不要归档** |

**科室主数据**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `DEPT_NOT_FOUND` | 科室主数据不一致 | 核对科室代码与 HIS 主数据；本轮结果标「科室主数据未校验」 |
| `DEPT_NAME_MISMATCH` | 科室名称写法与主数据不一致 | **只提示不改数**：核对名称写法，或把该写法报给信息科登记为别名 |
| `DEPT_DIRECTORY_UNAVAILABLE` | 科室主数据目录不可用 | 确认 `HIS_BASE_URL` / `HIS_API_TOKEN` 已配置且院内网段可达 |
| `DEPT_DIRECTORY_TIMEOUT` | 科室主数据目录调用超时 | 调大 `HIS_TIMEOUT_SECS`；**超时不算查过** |
| `DEPT_DIRECTORY_RESPONSE_INVALID` | 科室主数据目录响应结构不合法 | 目录响应应为数组且每条带 `dept_code`；找信息科核对接口实现 |

**模型端点（只影响重复项语义归并这一级）**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `MODEL_UNAVAILABLE` | 模型端点不可达 | **主路径仍可跑**；只标「疑似」，不合并不推送 |
| `MODEL_TIMEOUT` | 模型端点调用超时 | 调大 `OPENAI_TIMEOUT_SECS` 或减小单次候选对数；超时不算查过 |
| `MODEL_RESPONSE_INVALID` | 模型响应未通过结构校验 | 整次调用已作废；**本轮只留确定性候选，不写任何业务表** |

**状态库**

| 错误码 | 中文说明 | 处理动作 |
|--------|----------|----------|
| `DB_CORRUPTED` | 状态库文件损坏 | 先把损坏文件改名备份（**不要删**），按 4.4 的恢复流程处理 |
| `DB_VERSION_UNSUPPORTED` | 状态库版本不被本引擎识别 | 升级引擎到支持该状态的版本，或换一个状态库路径；**不要用新引擎直接改旧库** |
| `BATCH_SUMMARY_CONFLICT` | 批次汇总重放冲突 | 本次事务已整笔回滚，汇总回到批前值 —— 确认不是重复触发后再重跑 |
| `AUTHORITY_KEY_CONFLICT` | 权威键重复 | 同一机构+报告期+指标+科室+业务主键只能有一行权威数据；由经办人判定 |

> **完整错误码清单**（含每个码的中文说明与处理动作）以引擎的 `src/errors.rs` 为准；
> 表格插件弹窗显示的就是那里生成的内容。`regval messages --out messages.json`
> 可以导出一份 JSON 清单给科室看。

### 4.3 日志位置与保留策略

| 通道 | 内容 | 位置 |
|------|------|------|
| **stdout** | 给人看的摘要（校验行数、问题数、口径版本、批次号、耗时） | 终端 / 计划任务的重定向文件 |
| **stderr** | 结构化 JSON 行日志（一行一条 JSON，带 `ts` / `level` / `event` / `batch_ref` / `error_code`） | 同上 |
| 本机校验库 | 运行摘要（表数、行数、问题数、耗时）、确认记录、回执、审计链 | `REGVAL_STATE_DB` 指向的文件 |

**怎么收集日志**（计划任务或排障时）：

```bash
regval validate --input 科室表.csv --out result.json 2>> regval-$(date +%Y%m%d).log
```

**保留策略**：

| 留什么 | 留多久 | 理由 |
|--------|--------|------|
| 终端摘要（stdout） | 每个报告期归档一份 | 对账时要核「这批结论是按哪套口径算的」 |
| 结构化日志（stderr） | 3 个月，按月一个文件 | 排障（网关退回、回执冲突）一般不出一个月 |
| 本机校验库里的审计链与回执 | **按院内档案要求留存，不自动删** | 回执台账是对外报送的凭据，审计链要能回放 |
| 表格目录里的 `.regval_result.json` / `.regval_error.json` | 校验完可手工删 | 只是交接用的中间文件，不是档案 |

> 结构化日志里**不含患者可识别明细**，只有指标级与科室级的监管数据；
> 但里面仍含**行键与批次号**，按院内数据管理要求对待即可。

### 4.4 状态库备份与恢复

状态库是这台机器的「账本」—— 确认记录、回执、审计链都在里面。**坏了不要删**。

**备份**（每个报告期结束时做一次）：

```bash
regval status --state-db "D:\regval-state\state.db" --limit 5   # 先看看库是活的
copy "D:\regval-state\state.db" "D:\regval-backup\state-2026Q3.db"
```

**坏了怎么办**：

```bash
# 1) 先干跑：只打印恢复报告，不动原库
regval recover --state-db "D:\regval-state\state.db"

# 2) 确认恢复报告没问题再应用（原库会先被挪成 .corrupt-N 备份，不删）
regval recover --state-db "D:\regval-state\state.db" --apply --out 恢复报告.json
```

恢复按**幂等键与回执记录**恢复到最后一个一致状态。

**审计链回放**（给内审/飞检看「这一行走过哪几步」）：

```bash
regval audit --batch <批次号>                    # 回放某一批
regval audit --row <行键>                        # 回放某一行
regval audit --idempotency                       # 只核对幂等键 ↔ 回执 ID 一一对应
```

### 4.5 日常巡检清单

| 频率 | 做什么 | 命令 | 期望 |
|------|--------|------|------|
| 每次装/换版本 | 逐项自检 | `regval doctor` | `0 项缺失` |
| 每个报告期开始 | 确认规则版本 | `regval rules --dir ./rules` | 生效版本 = 质控岗确认过的版本 |
| 每个报告期开始 | 确认科室目录可用 | `regval dept` | 「目录 8 条 · 数据时间 …（实时拉取）」或明确标了「未校验」 |
| 每个报告期结束 | 跑一次回执台账导出 | `regval export receipts --out 回执台账.csv` | 「交叉核对 … 一一对应 成立」 |
| 每个报告期结束 | 备份状态库 | 见 4.4 | 备份文件存在且能打开 |
| 每月 | 核对幂等键与回执一一对应 | `regval audit --idempotency` | 无冲突 |

---

## 5. 最小可运行验证（2 分钟）

信息科在**自己机器上**第一次装完，用随包附带的脱敏合成样例跑一遍，确认这套东西在本机是活的。
**全程离线，不需要连院内任何系统**。

### 5.1 准备

```bash
cd <安装目录>
export REGVAL_STATE_DB=./.regval/state.db
export REGVAL_RULE_DIR=./rules
```

> 想把状态库放到别处就改 `REGVAL_STATE_DB`；不配也行，
> 但引擎会以 `CONFIG_MISSING` 退出并点名缺哪一项 —— 这是设计，不给默认值兜底。

### 5.2 一条命令

```bash
regval validate \
  --input samples-dataset/合并表.csv \
  --out result.json
```

### 5.3 期望输出与核对点

真实输出（1200 行样例；批次号与耗时每次不同）：

```text
批次汇总：已上报 0 行 · 被退回 0 行 · 待重试 0 行 · 问题 336 项
已校验 1200 行 · 336 项口径问题 · 48 项疑似重复待确认 → result.json
涉及 240 行有问题
读表格式：编码 mixed · 分隔符 半角逗号
行状态：1200 行 · 推进 972 · 未变 228 · 已上报受保护 0 · 退回修正回待确认 0 · 复校验退回 0
行状态分布：待校验 228 行 · 校验通过 972 行
重复检测漏斗：确定性键 12 对 · 归一化候选 12 对（未归一 115 行 · 与确定性键重叠 12 对 · 数值不同未收 18 对）· 编辑距离候选 24 对（阈值 2 · 超出阈值 0 对 · 长度差过大跳过 0 对）
距离分布：距离 1 24 对
口径规则集：版本 2021.1 · 生效 11 条 · 指纹 8125c3aaa14219ba
模型端点：本轮未接入（疑似项来源为确定性候选）
语义归并：模型提出 0 条 · 可合并候选 0 对 · 置信度不足待人工判定 0 对
批次 合并表-588aaf80cc56 · 本机库 ./.regval/state.db · 耗时 1098 ms
科室主数据未校验（未配置院内 HIS 科室目录（HIS_BASE_URL / HIS_API_TOKEN））
```

**逐条核对点**（对不上就是本机环境有问题）：

| 看什么 | 期望 | 对不上说明什么 |
|--------|------|----------------|
| 退出码 | `0` | 非 0 时看 stderr 里的错误码（第 4.2 节） |
| 「已校验 N 行」 | `1200` | 行数不对 → 读表环节跳过了行，看 `ingest.skipped_detail` |
| 口径问题项数 | `336`（其中 240 行有问题） | 数不对 → 规则目录版本不对，看下一行 |
| 口径规则集版本 | `版本 2021.1 · 生效 11 条` | 版本不对 → `REGVAL_RULE_DIR` 指错了目录 |
| 重复检测漏斗 | 确定性键 12 对 / 归一化 12 对 / 编辑距离 24 对 | 数字为 0 → 样例没读到，或规则目录不对 |
| 模型端点那行 | `本轮未接入` | 这是**正常**的：离线机没配模型端点，语义归并这一级留空，其余照跑 |
| 科室主数据那行 | `未校验` | 这是**正常**的：没配 HIS 目录。配了会变成「已校验 · 目录 N 条 · 数据时间 …」 |

**产物**：`result.json`（结果契约）。快速看两个数：

```bash
python3 -c "import json;d=json.load(open('result.json'));print(d['schema_version'], len(d['rows']), len(d['issues']))"
# → regval.contract/1.0.0 1200 336
```

### 5.4 三条进阶命令

```bash
# 看这批读表读成了什么样（编码、分隔符、跳过几行、状态分布）
regval status --state-db ./.regval/state.db

# 导出一份带官方依据的举证包（给科室看「为什么不合格、按哪条口径、该怎么改」）
regval export evidence --out 举证包.csv

# 看未上报的名册（催报用）
regval export pending --out 未上报.csv
```

### 5.5 出问题先看这张对照表

| 现象 | 最可能的原因 | 先做这一步 |
|------|--------------|------------|
| `regval: command not found` | 二进制没在 PATH，或宏里的引擎路径没改 | 把引擎绝对路径填进宏顶部常量；命令行则用绝对路径调 |
| `CONFIG_MISSING` | 环境变量没注入 | 跑 `regval doctor`，照每项下方的补法补 |
| 状态库写不进去 | 目录权限 / 域策略把用户目录设成只读 | 把 `REGVAL_STATE_DB` 指到本地磁盘（如 `D:\regval-state\state.db`） |
| 提示「引擎与宏版本不一致，请升级」 | 引擎与宏不是同一次交付 | 两者都换成同一次交付的版本 |
| 校验结果里科室维度写「未校验」 | 没配 HIS 目录（或拉不到） | 配 `HIS_BASE_URL` / `HIS_API_TOKEN`；拉不到时会退回上次缓存并标出数据时间 |
| 样例读出来「编码 mixed」 | 样例表本身混编（故意的脏数据用例） | 正常，引擎逐行判定 UTF-8 / GBK，计数留痕 |

---

## 6. 相关文档

| 文档 | 内容 |
|------|------|
| `README.md` | 产品说明：能力要点、快速开始、命令 / API、典型场景、输出样例 |
| `macro/README.md` | 宏的详细说明：三模块导入、WPS 装配、样式 token、常见问题 |
| `docs/architecture.md` | 架构与数据流：状态机、审计链、幂等键、失败恢复 |
| `docs/cli.md` | 全部子命令与参数参考 |
| `docs/examples.md` | 输出样例集（脱敏） |
| `src/errors.rs` | 错误码、中文说明与处理动作的**唯一出处** |
| `src/config.rs` | 配置项登记的**唯一出处**（doctor 输出与文档同源） |
