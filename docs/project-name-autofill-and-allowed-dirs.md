# 新建项目自动填名称 + `allowed_dirs` 静默丢失修复

> 2026-10-10 · 分支 `feat/project-name-autofill`（基线 main `c7418c5`）

## 一、需求主体：新建项目选目录自动填名称

「新建/编辑项目」共用一个弹窗（`ui/src/features/shell/ProjectNav.tsx`），表单只有**项目名称**与**项目主目录**两个字段，此前两字段完全无联动。CodeWave 的用户把项目主目录指向自己的代码仓库目录——仓库目录名几乎总是已经存在、且是他们唯一会脱口而出的那个名字，现状等于让每人每次重复敲一遍同一个字符串，且保存按钮在名称为空时禁用（用户得先通过一个自己不掌握的信息才能继续）。

改动落在 `pickDirectory()` 单点：

```ts
async function pickDirectory() {
  const dir = await ipc.selectWorkspaceDir();
  // 提前 return 是硬要求：baseName(null) 会因其 `|| p` 兜底得到字面量 "null"
  if (!dir) return;
  setProjDir(dir);
  if (editing === null && !projName.trim()) setProjName(baseName(dir));
}
```

**语义边界（勿自行「优化」这几条）**：

| 场景 | 行为 |
|---|---|
| 新建 + 名称空 + 选目录 | 填 `baseName(dir)` |
| 新建 + 已手输名称（含仅空格） | 不动（判据 `!projName.trim()` 与保存按钮禁用判据同表达式） |
| 再选一次目录 / 清目录 Tag 的叉 / 取消选择 | 名称**保留**，不再更新 |
| **编辑**已有项目选目录 | **完全不联动**，名称原样回填 |
| 名字与既有项目重名 | 原样填、不去重、不提示（左栏按 id 展示，允许并列） |
| 根目录 `C:\` | `baseName` 得 `"C:"`，原样用 |

三条设计要点：

1. **`if (!dir) return` 是硬要求**，不是风格问题。`baseName` 的实现是 `p.replace(/\\/g,"/").split("/").filter(Boolean).pop() || p`，那个 `|| p` 兜底会把 `null` 变成字符串 `"null"`——用户点「取消选择目录」就会把项目名填成 `null`。已有用例 `取消选择目录时名称不得变成字符串 null` 钉住。
2. **零状态**：用「名称非空即永不改动」这一纯条件表达「填一次后不覆盖」，**不需要** dirty/touched 标记位。
3. **零感知成本**：刻意不加任何提示文案（灰底/角标/Tooltip/placeholder 切换一律不加），自动填出的值与手输值视觉上完全一致。三个新建入口（左栏 `+`、会话空态引导、项目管理弹窗）都汇入同一 `pickDirectory`，故无需分别处理。

取名复用既有 `baseName()`（`ui/src/utils/path.ts`），**不要另写 split**——它已归一 Windows 反斜杠与 POSIX 分隔符、`filter(Boolean)` 处理尾斜杠，与 `TopBar` 显示项目名胶囊是同一套口径。

## 二、`allowed_dirs` 静默数据丢失（本批主缺陷）

### 现象

用户给某项目放行了主目录之外的「始终允许」目录后，只要在项目弹窗里改个名字（或改目录）点保存，那些目录全部消失，下次拖外部文件进来又被重新追问。

### 根因（三处叠加）

1. **类型层失守**：前端 `ProjectEntry`（`ui/src/ipc/types.ts`）**根本没有** `allowed_dirs` 字段，而 TS 的结构类型对手写字面量不做缺字段报错——零编译期信号。
2. **值层丢失**：`saveProject()` 手工重建 entry 字面量时逐字段抄回（`id`/`name`/`directory`/`data_dir`/`created_at`），**唯独漏抄** `allowed_dirs` → wire 上无该键。
3. **后端无合并**：`save_project` 是整份覆盖写，`#[serde(default)]` 把缺失键补成空数组 → 原值被静默清空。

「逐字段抄回」全靠人记得，此前 `data_dir` 与 `created_at` 都抄对了，唯独 `allowed_dirs` 漏了——**没有任何机制在加字段时提醒**。

### 修复：三态语义 + 独立入参 DTO

关键设计：**新增入参 DTO `ProjectSaveInput`**（仅 `Deserialize`），用 `Option<Vec<String>>` 表达三态：

| wire 形态 | 语义 |
|---|---|
| 缺该键 / `null` | 前端本次**未携带** → 沿用磁盘旧值 |
| `Some(v)` | 显式给值 → 用 `v` |
| `Some([])` | **显式清空** |

**落盘 DTO `ProjectEntry` 保持 `Vec<String>` 不变**——`project.json` 格式零变化，读回路径（`normalize`/`allow_dir`/`session.rs` 消费方）零波及，这是不把 `Option` 语义渗透进全仓的关键。

配套前端约束（**勿写 `?? []`**）：

```ts
// 刻意不写 `?? []`：undefined 不上 wire → 后端识别为「未携带」→ 沿用磁盘旧值
allowed_dirs: editing?.allowed_dirs,
```

`undefined` 在对象字面量里确实是值为 `undefined 的自有键`，但 `JSON.stringify` 的规范行为就是丢弃值为 `undefined` 的对象属性，Tauri IPC 的 `processIpcMessage` 正是 `JSON.stringify(message, replacer)`（replacer 只特判 `Map`/`Uint8Array`/`ArrayBuffer`/`SERIALIZE_TO_IPC_FN`，对 `undefined` 走 `else { return val }`）——**wire 上确实不存在该键**，后端 `#[serde(default)]` 得到 `None`。三态由 `allowed_dirs_omitted_keeps_previous` / `allowed_dirs_explicit_empty_clears` 两条用例钉死。

### 合并读哪一份旧值

私有 `read_existing()`：**只读** `directories.json` 索引按 id 反查旧主目录 → **只读那一个** `project.json`。三条理由：

1. **必须经索引**：编辑时用户可能改了主目录，旧 `project.json` 在**旧**目录之下。
2. **不走 `find()`/`load()`**：后者是全量 load，且内部会写索引（`index_forget`/`index_remember`），而 `create_session` 是 find+save 热路径。
3. **带 id 匹配防御**：索引指向的 `project.json` 里若是另一个项目 id，一律当作「无旧值」（回落空列表）——索引与内容不一致时**宁可不放行，也不继承别的项目的目录**。

## 三、相邻缺陷：改主目录致项目从左栏消失

编辑项目时同时改主目录：`data_dir` 被原样保留在**旧**目录（`editing?.data_dir ?? ...`），于是 `project.json` 被写回**旧**目录、`directories.json` 索引却指向**新**目录 → 重启后 `load()` 在旧目录读不到 → **项目既不可见也删不掉**。

修复在 `normalize_data_dir` 追加第三条分支（抽成 `save_project` 与 `save_project_input` 共用的函数）：

- 判据 = **`data_dir` 的父目录 ≠ 当前传入的 `directory`**（即主目录被改过）→ 重算为 `<新 directory>/.codewave`。
- 比较时两侧都先过 `comparable_path`（能 `canonicalize` 就用其绝对形态，否则退回原样）。**Windows 可靠性的依据**：canonicalize 返回 `\\?\` verbatim 路径，与普通字符串直接比较永远不相等；两侧都 canonicalize 后前缀与大小写都自动归一。canonicalize 失败（路径不存在）不报错，判成「不跟随」→ 触发迁移，这正是想要的方向。
- 全程只做 `Path::parent()` 与 `PathBuf` 比较，**不做字符串拼接**（Windows 上正斜杠字符串直接拼会 `ERROR_INVALID_NAME`）。

## 四、相邻缺陷：空主目录污染进程 CWD

`save_project` 此前不校验 `directory`，空串时 `project_data_dir` 落到相对路径 `.codewave` → `create_dir_all` 在**进程工作目录**下建目录，且 `index_remember` 被 `!entry.directory.is_empty()` 跳过 → 该项目永久不可见也删不掉。

修复：`save_project_input` 在**任何 `create_dir_all` 之前**对空/纯空白 `directory` 直接 bail。

## 五、后端校验分层（L5 收口）

历史缺陷 L5（登记于 [quality-and-feature-batch-report](./quality-and-feature-batch-report.md)）：`save_project` 除 `valid_id` 外不校验任何字段。收口为三层：

| 层 | 函数 | 管什么 | 为什么在这 |
|---|---|---|---|
| host 命令 | `validate_save_input(name, directory)` | **用户输入准入**：名称/主目录非空 | AGENTS.md 分层：IPC 命令只做校验 + 转调 core |
| core 兜底 | `save_project_input` 内的 directory 检查 | **结构性非法**：防 CWD 污染 | 挡住将来绕过 host 的内部调用路径 |
| core 不拦 | `save_project` 内部入口**不校验 name** | —— | `create_session` 会拿磁盘 entry 回写 `save_project`；用户磁盘上**可能存在历史空名项目**，core 层硬 bail 会让它们**建不出会话** |

命令体的「先校验、再转调」抽成同步私有函数 `persist_project()`，让**接线顺序本身进测试边界**——只测 core 纯函数的话，把命令体里这行删掉仍然全绿。（沿用同文件 `apply_page_save_shape` 的同一用意：原缺陷是「入口少做了一次」。）

## 六、契约测试（缺陷能长期存活的根因）

仓库此前有事件面的 `events.contract.test.ts` 双向守护，但**没有** project.json 字段集合的守护——这正是本缺陷能活到今天的原因。

- **Rust**（`core/projects.rs`）：`project_json_wire_field_set_contract` 钉死落盘 JSON 的 6 键集合（硬锚点，后端加字段必须同步改）。
- **TS**（`ui/src/__tests__/projects.contract.test.ts`）：仿 `events.contract.test.ts` 手法双向对拍 Rust struct 字段 ↔ TS interface 字段 + 6 字段硬锚点。`rustFields` 刻意**扫描全部同名 struct 起点**而非取首个——本文件里紧挨着还有入参 DTO `ProjectSaveInput`，其字段名是 `ProjectEntry` 的真子集，若将来出现 `ProjectSaveInputV2` 之类命名，首匹配策略会假绿。

## 七、测试清单

Rust（`core/projects.rs` 新增 9 条 + 既有 5 条）：

| 用例 | 防什么 |
|---|---|
| `project_json_wire_field_set_contract` | 落盘 6 键集合漂移 |
| `allowed_dirs_omitted_keeps_previous` | 本批主缺陷：缺键不得清空用户已放行的目录 |
| `allowed_dirs_explicit_empty_clears` | 三态第三态不被合并吃掉 |
| `merge_never_crosses_ids` | A 的放行目录不被 B 继承（越权放行）+ `read_existing` 的 id 匹配防御 |
| `directory_change_moves_data_dir_and_keeps_project` | 一条盖两个缺陷：改目录后 allowed_dirs 保留 + data_dir 迁移 + `load()` 仍能发现 |
| `empty_directory_rejected_without_creating_dirs` | 空目录在任何 mkdir 之前被拒 |
| `legacy_empty_name_project_still_savable` | **N4 兼容护栏**：历史空名项目经内部回写仍成功 |
| `validate_save_input_rejects_empty_name_and_directory` | 校验四分支 |
| `save_project_input_rejects_invalid_id_before_any_read` | id 校验先于任何读盘（防目录穿越触发读取） |

host（`host/commands/project.rs` 新增 1 条）：`save_project_command_validates_before_writing` —— 删掉命令体的校验接线即变红。

TS 新增 11 条：9 条行为（自动填 / 已填不覆盖 / 仅空白视为空 / 二次选目录不改名 / 清 Tag 保留 / 取消不填 `null` / 编辑不联动 / 编辑透传 `allowed_dirs` / 新建不带 `allowed_dirs`）+ 2 条契约。

**断言纪律**：涉及 save 侧行为的用例**直接读落盘 JSON**，不要只靠 `load()`/`find()`——`load()` 会用索引无条件重算 `data_dir`，用它断言 save 侧行为会**假绿**。

## 八、遗留与已知边界

- **旧 `.codewave` 目录不自动清理**：主目录变更后新位置会重建托管目录，但**旧位置的数据保留不删**（自动删除用户目录属危险行为）。如需迁移旧数据或提示用户手动清理，另开批次。
- **`comparable_path` 两侧都失败时按字面比较**：`D:\Code\a` 与 `D:/Code/a` 这类分隔符差异会误判为「主目录已变」→ 触发一次**幂等重写**（重算值与原值等价，无数据损失）。
- **host 层校验接线已有用例**；`read_existing` 的 id 不一致防御、`normalize_data_dir` 的 `stale_legacy` 分支当前无独立正例用例（前者已被 `merge_never_crosses_ids` 后半段覆盖，后者属 legacy 清理范畴）。

## 九、验证

- `cargo test`：1206 passed / 0 failed / 3 ignored（`cfg(unix)`），**0 warning**
- `cargo fmt --check` / `cargo clippy --workspace --all-targets`：干净
- `pnpm --dir ui test`：1294 passed / 107 文件；`lint` 与 `build`（含 tsc type check）通过
- 判别力反向验证（改坏实现确认测试变红，已还原）：合并逻辑改回覆盖写 → `allowed_dirs_omitted_keeps_previous` 变红；自动填那行短路为永假 → 5 条前端用例变红；注释掉空目录 bail → `empty_directory_rejected_without_creating_dirs` 变红（且实测产生了真实的 `src-tauri/.codewave` 目录污染，正是该用例要挡的现象）；注释掉命令体校验接线 → `save_project_command_validates_before_writing` 变红。
- UI 改动不做 GUI 自动点验 → 见下节手动验证清单。

## 十、手动验证清单（交付用户）

1. 左栏 `+` → 新建项目弹窗 → **不填名称** → 点「选择目录」选一个目录（如 `D:\Code\Personal\CodeWave`）→ 名称框应自动出现 `CodeWave`，保存按钮可点。
2. 先在名称框填「我的项目」→ 再选目录 → 名称应**仍是**「我的项目」。
3. 点「选择目录」后点**取消** → 名称框应**仍为空**，绝不能出现字符串 `null`。
4. 自动填名后再选一次别的目录 → 名称**不应改变**。
5. 点目录 Tag 的 ✕ → 名称**保留**、保存按钮禁用；再选目录 → 名称**不更新**。
6. 「管理项目」→ 编辑某个项目 → 改主目录 → 名称**不应联动**；保存后重启应用，该项目**仍应出现在左栏**（验证 N1 修复）。
7. 若之前给某项目放行过外部目录（拖外部文件时选过「始终允许」）→ 编辑该项目改个名字保存 → 下次拖该目录的文件进去**不应再被追问**。