//! 工具入参 fuzz 测试（[docs/p0-plan](../../../docs/p0-plan.md) §13 质量底线：无 panic 路径）。
//! 确定性短跑版本（固定 LCG 种子，500 轮 × 多种畸形输入形态），验证参数解析层
//!（serde 解析 / parse_or_salvage / collect_unknown_fields）在任何输入下都不 panic。

use crate::core::sessions::repair::parse_or_salvage;
use crate::tools::{ToolOutcome, collect_unknown_fields};

/// 线性同余伪随机数生成器：固定种子即可复现，满足测试确定性要求。
struct Lcg(u64);
impl Lcg {
    /// 产出下一个伪随机数（取高位，避免低位短周期）。
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
}

/// 对基准 JSON 做随机变异：插入危险字符、删除/替换字符、截断（模拟 SSE 分片撕裂等真实畸形来源）。
fn mutate(base: &str, rnd: &mut Lcg) -> String {
    let bytes: Vec<char> = base.chars().collect();
    let mut out: Vec<char> = bytes.clone();
    for _ in 0..(rnd.next() % 5 + 1) {
        let pos = (rnd.next() as usize) % out.len().max(1);
        match rnd.next() % 4 {
            0 => out.insert(
                pos.min(out.len()),
                ['#', '"', '\\', '{', '}'][(rnd.next() % 5) as usize],
            ),
            1 => {
                if !out.is_empty() {
                    out.remove(pos.min(out.len() - 1));
                }
            }
            2 => {
                if !out.is_empty() {
                    let p = pos.min(out.len() - 1);
                    out[p] = (rnd.next() % 128) as u8 as char;
                }
            }
            _ => {
                let cut = pos.min(out.len());
                out.truncate(cut); // 模拟截断
            }
        }
        if out.is_empty() {
            break;
        }
    }
    out.into_iter().collect()
}

#[test]
fn arg_parsers_never_panic_under_mutation() {
    let bases = [
        r#"{"files":[{"path":"a.txt","version":"AB12CD","changes":[{"oldText":"x","newText":"y"}]}]}"#,
        r#"{"command":"echo hi","timeoutSeconds":120}"#,
        r#"{"pattern":"TODO","maxMatches":100,"offset":5}"#,
        r#"{"path":"x/y","recursive":true}"#,
        r#"{"questions":[{"id":"q1","question":"?","options":[{"id":"a","label":"A"}]}]}"#,
        r#"{"path":"src","maxDepth":3}"#,
    ];
    let schema =
        r#"{"properties":{"files":{},"command":{},"pattern":{},"path":{},"questions":{}}}"#;
    let mut rnd = Lcg(0x5741_5645_5354_5544);
    for round in 0..500 {
        let base = bases[(rnd.next() as usize) % bases.len()];
        let mutated = mutate(base, &mut rnd);
        // 宽松解码路径不得 panic（结果正确与否无关紧要）
        let _v: Result<serde_json::Value, _> = serde_json::from_str(&mutated);

        // 三条解析路径均不允许 panic（结果正确与否无关紧要）
        let parsed: Option<serde_json::Value> = serde_json::from_str(&mutated).ok();
        let _ = parse_or_salvage(&mutated);
        let _ = collect_unknown_fields(&parsed.clone().unwrap_or(serde_json::Value::Null), schema);
        // ToolOutcome 序列化回环
        let out = match parsed {
            Some(v) => ToolOutcome::ok(v),
            None => ToolOutcome::err("E_ARGS", format!("round {round}")),
        };
        let _ = serde_json::to_string(&out).unwrap();

        // 中毒历史的注入解析器同样不得 panic
        let _ = parse_or_salvage(&(base.to_string() + &mutated));
    }
}

/// LCG 是确定性生成器：同一种子重放同一序列。
#[test]
fn lcg_is_deterministic_per_seed() {
    let mut a = Lcg(0x5741_5645_5354_5544);
    let mut b = Lcg(0x5741_5645_5354_5544);
    let mut c = Lcg(1);
    let (mut seq_a, mut seq_b, mut seq_c) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..32 {
        seq_a.push(a.next());
        seq_b.push(b.next());
        seq_c.push(c.next());
    }
    assert_eq!(seq_a, seq_b);
    assert_ne!(seq_a, seq_c);
}

/// 退化基准（空 / 单字符 / 多字节 unicode）的变异永不 panic。
#[test]
fn mutate_handles_degenerate_bases() {
    let mut rnd = Lcg(99);
    for base in ["", "x", "中文🚀\"{\\}"] {
        for _ in 0..200 {
            let out = mutate(base, &mut rnd);
            // 变异器只会插入/替换 ASCII 控制区字符或截断
            assert!(out.chars().count() <= base.chars().count() + 20);
            // 对变异后文本做 salvage + 未知字段收集保持无 panic
            let parsed: Option<serde_json::Value> = serde_json::from_str(&out).ok();
            let _ = parse_or_salvage(&out);
            let _ = collect_unknown_fields(
                &parsed.unwrap_or(serde_json::Value::Null),
                r#"{"properties":{"a":{}}}"#,
            );
        }
    }
}
