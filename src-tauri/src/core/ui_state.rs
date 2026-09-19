//! 全局 UI 现场态（会话保存与恢复优化 · 批1，需求共识 17/18）：`<data_dir>/ui-state.json`。
//! Tab 集合/顺序/活跃 Tab/滚动锚点/草稿/队列/树展开态/面板态/窗口几何的唯一持久化事实源
//!（主题/语言/字体/左右栏开合仍留在 localStorage，两类不双写）。
//!
//! Rust 侧只校验顶层 `schema` 版本与体积上限，其余内容**整块透传**——字段级 schema 由前端维护，
//! 后端不做结构理解（前端演进字段不影响落盘）。损坏或版本不匹配先备份原文件再回默认，
//! 绝不静默删除（E2/E6）。
//! 分层规则：本层不依赖 tauri。

use crate::util::atomic::atomic_write;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 支持的 ui-state schema 版本（顶层 `schema` 字段；不匹配 → 备份为 `.v<N>.bak` 后回默认）。
pub const SCHEMA_VERSION: u64 = 1;
/// 单文件字节上限（8MB）。读取超限按损坏处理；写入超限直接报错（裁项由前端负责，E3）。
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// 文件名（位于数据根目录）。
pub const FILE_NAME: &str = "ui-state.json";
/// 窗口尺寸合理下限（逻辑像素；小于此值视为异常数据，抬到下限）。
/// 与 `tauri.conf.json` 的 `minWidth` 保持一致：左栏 180 + 中栏 480 + 右栏 328 = 988，
/// 取 1024 留余量（可拖拽栏宽需求，见 docs/rightbar-info-refactor-and-subscription-quota）。
pub const MIN_WINDOW_WIDTH: f64 = 1024.0;
/// 窗口高度合理下限（逻辑像素）。
pub const MIN_WINDOW_HEIGHT: f64 = 480.0;

/// ui-state 文件路径：`<data_dir>/ui-state.json`。
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// 读取 ui-state。
/// - 文件不存在 → `None`（首次启动/前端尚未落盘，属正常路径，不产生备份）
/// - 读元数据/内容失败、JSON 非法、schema ≠ 1、超体积 → 备份原文件后回 `None`
///
/// 任何情况下都不删除原文件：备份是**移动**（原路径不得残留损坏文件，否则下次启动仍命中）。
pub fn load(data_dir: &Path) -> Option<Value> {
    let file = path(data_dir);
    let meta = match std::fs::metadata(&file) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("ui-state 元数据读取失败（{}）：{e}", file.display());
            return None;
        }
    };
    if meta.len() > MAX_BYTES as u64 {
        backup(&file, None);
        tracing::warn!(
            "ui-state 超过 {MAX_BYTES} 字节上限（{} 字节），已备份并回默认",
            meta.len()
        );
        return None;
    }
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("ui-state 读取失败（{}）：{e}", file.display());
            return None;
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            backup(&file, None);
            tracing::warn!("ui-state JSON 非法，已备份并回默认：{e}");
            return None;
        }
    };
    match value.get("schema").and_then(Value::as_u64) {
        Some(v) if v == SCHEMA_VERSION => Some(value),
        other => {
            backup(&file, other);
            tracing::warn!(
                "ui-state schema 不匹配（{other:?}，期望 {SCHEMA_VERSION}），已备份并回默认"
            );
            None
        }
    }
}

/// 写入 ui-state：校验顶层 `schema == 1` 与体积上限，再经 `atomic_write` 原子落盘。
/// 磁盘错误向上传递（E9：写失败绝不静默；前端据此提示并保留内存态）。
pub fn save(data_dir: &Path, state: &Value) -> anyhow::Result<()> {
    match state.get("schema").and_then(Value::as_u64) {
        Some(v) if v == SCHEMA_VERSION => {}
        other => anyhow::bail!("ui-state schema 必须为 {SCHEMA_VERSION}（收到 {other:?}）"),
    }
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() > MAX_BYTES {
        anyhow::bail!(
            "ui-state 超过 {MAX_BYTES} 字节上限（{} 字节），已拒绝写入",
            bytes.len()
        );
    }
    atomic_write(&path(data_dir), &bytes)?;
    Ok(())
}

/// 备份原文件：版本可识别 → `<名>.v<N>.bak`（E6），否则 `<名>.corrupt`（与索引损坏备份同一约定）。
/// 备份失败只告警并保留原文件（宁可下次再判损坏，也不丢用户的现场态）。
fn backup(file: &Path, version: Option<u64>) {
    let name = file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(FILE_NAME);
    let target = match version {
        Some(v) => file.with_file_name(format!("{name}.v{v}.bak")),
        None => file.with_file_name(format!("{name}.corrupt")),
    };
    // 覆盖旧备份（Windows 上 rename 目标已存在会失败 → 先删）
    let _ = std::fs::remove_file(&target);
    if std::fs::rename(file, &target).is_err() {
        // rename 失败（跨文件系统等）：退化为复制 + 删除原文件
        if std::fs::copy(file, &target).is_ok() {
            let _ = std::fs::remove_file(file);
        } else {
            tracing::warn!(
                "ui-state 备份失败（{} → {}），原文件保留原地",
                file.display(),
                target.display()
            );
            return;
        }
    }
    tracing::info!("ui-state 已备份到 {}", target.display());
}

// ---------- 窗口几何（恢复范围含窗口尺寸与位置，共识 16；E5 显示器变更回落） ----------

/// 窗口几何（逻辑像素；`x`/`y` 缺失 = 只记了尺寸，位置由系统决定）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeometry {
    /// 宽度
    pub width: f64,
    /// 高度
    pub height: f64,
    /// 左上角 x（None = 未记录）
    pub x: Option<f64>,
    /// 左上角 y（None = 未记录）
    pub y: Option<f64>,
}

/// 显示器工作区（逻辑像素，与前端记录的 CSS 像素同单位）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkArea {
    /// 工作区左上角 x
    pub x: f64,
    /// 工作区左上角 y
    pub y: f64,
    /// 工作区宽度
    pub width: f64,
    /// 工作区高度
    pub height: f64,
}

impl WorkArea {
    /// 点是否落在工作区内（窗口左上角判定）。
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// 读 ui-state 的 `window` 节点（`{ width, height, x, y }`，逻辑像素）。
/// 节点缺失、宽/高缺失或非正数 → `None`（调用方跳过几何恢复，保持配置默认）。
pub fn window_geometry(state: &Value) -> Option<WindowGeometry> {
    let node = state.get("window")?;
    let width = node.get("width").and_then(Value::as_f64)?;
    let height = node.get("height").and_then(Value::as_f64)?;
    // 非有限值与不大于 0 的尺寸一并拒绝（JSON 无 NaN，此处的 finite 检查是防御性的）
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(WindowGeometry {
        width,
        height,
        x: node.get("x").and_then(Value::as_f64),
        y: node.get("y").and_then(Value::as_f64),
    })
}

/// 校验并裁决窗口落点：尺寸不小于下限、不超主显示器工作区；位置不在任何工作区内（显示器变更/
/// 拔线）或未记录 → 回落主显示器（缺省取首个）居中。返回值恒为可直接应用的几何。
pub fn resolve_window_geometry(
    desired: WindowGeometry,
    work_areas: &[WorkArea],
    primary: Option<WorkArea>,
) -> WindowGeometry {
    let primary = primary.or_else(|| work_areas.first().copied());
    let mut width = desired.width.max(MIN_WINDOW_WIDTH);
    let mut height = desired.height.max(MIN_WINDOW_HEIGHT);
    if let Some(a) = primary {
        width = width.min(a.width.max(MIN_WINDOW_WIDTH));
        height = height.min(a.height.max(MIN_WINDOW_HEIGHT));
    }
    if let (Some(x), Some(y)) = (desired.x, desired.y) {
        if work_areas.iter().any(|a| a.contains(x, y)) {
            return WindowGeometry {
                width,
                height,
                x: Some(x),
                y: Some(y),
            };
        }
        tracing::info!("ui-state 窗口位置 ({x}, {y}) 不在任何显示器工作区内，回落主显示器居中");
    }
    match primary {
        Some(a) => WindowGeometry {
            width,
            height,
            x: Some(a.x + ((a.width - width) / 2.0).max(0.0)),
            y: Some(a.y + ((a.height - height) / 2.0).max(0.0)),
        },
        // 无显示器信息（极端环境）：只应用尺寸，位置交给系统
        None => WindowGeometry {
            width,
            height,
            x: None,
            y: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 合法 schema v1 的最小现场态。
    fn state() -> Value {
        json!({ "schema": 1, "tabs": ["t1"], "activeProject": null })
    }

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn round_trip_leaves_no_temp_files() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), &state()).unwrap();
        assert_eq!(load(d.path()).unwrap(), state());
        // 原子写不留临时文件（目录里只有 ui-state.json）
        assert_eq!(dir_names(d.path()), vec![FILE_NAME.to_string()]);
        // 覆盖写同样干净
        let mut next = state();
        next["drafts"] = json!({ "t1": "半段草稿" });
        save(d.path(), &next).unwrap();
        assert_eq!(load(d.path()).unwrap(), next);
        assert_eq!(dir_names(d.path()), vec![FILE_NAME.to_string()]);
    }

    #[test]
    fn missing_file_is_none_and_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path()).is_none());
        assert!(dir_names(d.path()).is_empty(), "读缺失文件不得留下任何痕迹");
    }

    #[test]
    fn corrupt_json_backed_up_as_corrupt() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(path(d.path()), b"{ not json").unwrap();
        assert!(load(d.path()).is_none());
        assert!(
            !path(d.path()).exists(),
            "损坏文件必须被移走备份，不得原地留存"
        );
        assert!(d.path().join("ui-state.json.corrupt").exists());
    }

    #[test]
    fn future_schema_backed_up_with_version_suffix() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(path(d.path()), br#"{"schema":2,"tabs":[]}"#).unwrap();
        assert!(load(d.path()).is_none());
        assert!(d.path().join("ui-state.json.v2.bak").exists());
        assert!(!d.path().join("ui-state.json.corrupt").exists());
    }

    #[test]
    fn missing_or_illtyped_schema_treated_as_corrupt() {
        for payload in [
            br#"{"tabs":[]}"#.as_slice(),
            br#"{"schema":"1"}"#.as_slice(),
            br#"[]"#.as_slice(),
        ] {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(path(d.path()), payload).unwrap();
            assert!(load(d.path()).is_none(), "payload={payload:?}");
            assert!(d.path().join("ui-state.json.corrupt").exists());
        }
    }

    #[test]
    fn oversize_write_rejected_without_touching_existing_file() {
        let d = tempfile::tempdir().unwrap();
        save(d.path(), &state()).unwrap();
        let huge = json!({ "schema": 1, "blob": "x".repeat(MAX_BYTES + 1) });
        let err = save(d.path(), &huge).unwrap_err();
        assert!(
            err.to_string().contains("上限"),
            "错误信息应说明体积上限：{err}"
        );
        // 原文件未被破坏（仍是上一次的合法内容）
        assert_eq!(load(d.path()).unwrap(), state());
        assert_eq!(dir_names(d.path()), vec![FILE_NAME.to_string()]);
    }

    #[test]
    fn oversize_file_backed_up_on_read() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(path(d.path()), vec![b'x'; MAX_BYTES + 8]).unwrap();
        assert!(load(d.path()).is_none());
        assert!(d.path().join("ui-state.json.corrupt").exists());
        assert!(!path(d.path()).exists());
    }

    #[test]
    fn save_rejects_bad_schema_and_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert!(save(d.path(), &json!({ "schema": 2 })).is_err());
        assert!(save(d.path(), &json!({})).is_err());
        assert!(save(d.path(), &json!({ "schema": "1" })).is_err());
        assert!(!path(d.path()).exists());
    }

    #[test]
    fn window_geometry_requires_positive_size_and_optional_position() {
        let full = json!({ "window": { "width": 1200.0, "height": 900.0, "x": 10.0, "y": 20.0 } });
        assert_eq!(
            window_geometry(&full).unwrap(),
            WindowGeometry {
                width: 1200.0,
                height: 900.0,
                x: Some(10.0),
                y: Some(20.0)
            }
        );
        // 只记尺寸：位置缺省
        let sized =
            window_geometry(&json!({ "window": { "width": 1000, "height": 700 } })).unwrap();
        assert_eq!((sized.x, sized.y), (None, None));
        // 缺失/类型错/非正数 → None
        assert!(window_geometry(&json!({})).is_none());
        assert!(window_geometry(&json!({ "window": { "height": 700 } })).is_none());
        assert!(
            window_geometry(&json!({ "window": { "width": "1000", "height": 700 } })).is_none()
        );
        assert!(window_geometry(&json!({ "window": { "width": -100, "height": 700 } })).is_none());
        assert!(window_geometry(&json!({ "window": { "width": 1000, "height": 0 } })).is_none());
    }

    #[test]
    fn undersized_size_clamped_but_in_bounds_position_kept() {
        let areas = [
            WorkArea {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            WorkArea {
                x: 1920.0,
                y: 0.0,
                width: 1280.0,
                height: 1024.0,
            },
        ];
        let placed = resolve_window_geometry(
            WindowGeometry {
                width: 200.0,
                height: 100.0,
                x: Some(2000.0),
                y: Some(100.0),
            },
            &areas,
            Some(areas[0]),
        );
        assert_eq!(
            (placed.width, placed.height),
            (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)
        );
        assert_eq!((placed.x, placed.y), (Some(2000.0), Some(100.0)));
    }

    #[test]
    fn out_of_bounds_or_missing_position_falls_back_to_primary_center() {
        let areas = [
            WorkArea {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            WorkArea {
                x: 1920.0,
                y: 0.0,
                width: 1280.0,
                height: 1024.0,
            },
        ];
        // 显示器变更：原点跑到屏幕外 → 主显示器居中
        let placed = resolve_window_geometry(
            WindowGeometry {
                width: 1200.0,
                height: 800.0,
                x: Some(-5000.0),
                y: Some(-5000.0),
            },
            &areas,
            Some(areas[0]),
        );
        assert_eq!((placed.x, placed.y), (Some(360.0), Some(140.0)));
        // 未记录位置 → 主显示器居中
        let placed = resolve_window_geometry(
            WindowGeometry {
                width: 1200.0,
                height: 800.0,
                x: None,
                y: None,
            },
            &areas,
            Some(areas[0]),
        );
        assert_eq!((placed.x, placed.y), (Some(360.0), Some(140.0)));
        // 位置刚好落在工作区右下边界外（半像素级越界）同样回落
        let placed = resolve_window_geometry(
            WindowGeometry {
                width: 1200.0,
                height: 800.0,
                x: Some(1920.0),
                y: Some(1024.0),
            },
            &areas,
            Some(areas[0]),
        );
        assert_eq!((placed.x, placed.y), (Some(360.0), Some(140.0)));
    }

    #[test]
    fn size_clamped_to_primary_work_area() {
        let areas = [WorkArea {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
        }];
        let placed = resolve_window_geometry(
            WindowGeometry {
                width: 3000.0,
                height: 2000.0,
                x: Some(10.0),
                y: Some(10.0),
            },
            &areas,
            Some(areas[0]),
        );
        assert_eq!((placed.width, placed.height), (1280.0, 720.0));
        assert_eq!((placed.x, placed.y), (Some(10.0), Some(10.0)));
    }

    #[test]
    fn no_monitor_info_keeps_size_without_position() {
        let placed = resolve_window_geometry(
            WindowGeometry {
                width: 1200.0,
                height: 900.0,
                x: Some(10.0),
                y: Some(10.0),
            },
            &[],
            None,
        );
        assert_eq!((placed.width, placed.height), (1200.0, 900.0));
        assert_eq!((placed.x, placed.y), (None, None));
    }

    #[test]
    fn work_area_contains_is_half_open() {
        let a = WorkArea {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        };
        assert!(a.contains(0.0, 0.0));
        assert!(a.contains(99.9, 49.9));
        assert!(!a.contains(100.0, 10.0));
        assert!(!a.contains(10.0, 50.0));
        assert!(!a.contains(-0.1, 10.0));
    }
}
