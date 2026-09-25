//! 文本形态 ask 的兜底恢复（[docs/text-form-ask-fallback]）。
//!
//! 场景：BYOK 端点把工具调用当作**正文**透传——模型那一轮没有产出任何 `tool_use`，
//! 正文末尾却漂着一段 `<ask>…</ask>`（实测会话 5da292d8：MiniMax 兼容端点漏出，
//! 字段值里还混入私有控制 token `]<]minimax[>[<`）。此前这段协议原文被当普通 Markdown
//! 渲染：问题既不弹卡、也无任何提示，用户只看到裸 XML。
//!
//! 本模块**只做纯函数搬运**（原文 → ask 入参 JSON + 被剥离的块文本），不产生任何副作用、
//! 不感知会话状态——是否兜底由调用方（`core/agent/drive.rs`）按条件门决定。
//! 判定一律保守：**块级**问题（未闭合、多块并存、块后仍有正文、非行首、代码围栏内、
//! 一题都没解析出来、题数超上限）整条放弃、退回既有行为；**单项**残缺（某题缺 id/question、
//! 某选项缺 id/label、某选项 mode 非法、某题选项超上限）只丢该项——应答协议按 id 关联
//!（`answers[qid].selections`），没有 id 的项本来就答不了，丢掉它比整条不兜底更贴近用户预期。

use serde_json::{Map, Value, json};

/// 题目/选项上限，与 `tools/ask/tool.rs` 的入参 schema 一致。
/// 题数超上限 → 整条放弃（静默少给几道题，不如让模型重发）；选项数超上限 → 丢该题
///（截断选项会改变题意）。
const MAX_QUESTIONS: usize = 5;
const MAX_OPTIONS: usize = 6;
/// 私有控制 token 的 name 段长度上限（`<]name[>`）。
const CONTROL_TOKEN_NAME_MAX: usize = 32;

/// 一次成功的文本形态 ask 恢复。
#[derive(Debug, Clone, PartialEq)]
pub struct TextAsk {
    /// 等价于 ask 工具入参的 JSON（可直接喂给 `AskTool`）。
    pub args: Value,
    /// 被恢复的原始块（含 `<ask>` / `</ask>` 标记）：调用方据此从正文中剥离。
    pub block: String,
}

/// 在正文里找**位于末尾**且结构完整的 `<ask>…</ask>` 并解析为 ask 入参（等价于工具 args）。
///
/// 判据（全满足才返回 `Some`）：
/// 1. 存在 `<ask>`（开标签可有属性，一律忽略）起、`</ask>` 止的块，块内不再有第二个 `<ask`；
/// 2. 块**独占起始行**（行首即块，或前一字符是换行）——行内代码 / 引述里的完整示例不触发；
/// 3. 块之后只剩空白——模型把调用写在正文末尾，而「讲解协议」的正文后面通常还有话，
///    误伤面因此收窄；
/// 4. 块不在 Markdown 代码围栏内（围栏里的是示例，不是调用）；
/// 5. 至少解析出 1 题（题内残缺项按单项丢弃，见模块文档）；题数 ≤ 5、每题选项 ≤ 6。
pub fn salvage_text_ask(text: &str) -> Option<TextAsk> {
    let (block_start, inner, block_end) = locate_block(text)?;
    if !text[block_end..].trim().is_empty() {
        return None;
    }
    // 块必须独占起始行：把兜底限制在「模型把自己要问的问题写成独占段落」这一形态上，
    // 行内引述的示例（`格式是 <ask>…`）因此不会弹卡。
    if block_start > 0
        && !text[..block_start]
            .chars()
            .next_back()
            .is_none_or(|c| c == char::from(10))
    {
        return None;
    }
    if inside_fence(text, block_start) {
        return None;
    }
    let args = parse_ask(&inner)?;
    Some(TextAsk {
        args,
        block: text[block_start..block_end].to_string(),
    })
}

/// 从正文中剥离指定块；只裁掉剥离后残留的**尾部**空白，不动其余内容。
pub fn strip_block(text: &str, block: &str) -> String {
    text.replacen(block, "", 1).trim_end().to_string()
}

/// 定位块：返回 (块起始字节, 块内文本, 块结束字节)。
fn locate_block(text: &str) -> Option<(usize, String, usize)> {
    let start = text.find("<ask")?;
    let rest = &text[start + 4..];
    // 标签名边界：`<ask>` 或 `<ask …>`（属性忽略）；`<asking>` 这类不算
    let name_ok = match rest.chars().next() {
        Some('>') => true,
        Some(c) => c.is_whitespace(),
        None => false,
    };
    if !name_ok {
        return None;
    }
    let gt = rest.find('>')?;
    let inner_start = start + 4 + gt + 1;
    let close_rel = text[inner_start..].find("</ask>")?;
    let inner_end = inner_start + close_rel;
    let block_end = inner_end + "</ask>".len();
    // 多块并存 = 歧义，放弃
    if text[inner_start..inner_end].contains("<ask") {
        return None;
    }
    Some((start, text[inner_start..inner_end].to_string(), block_end))
}

/// 解析块内文本为 ask 入参。字段顺序无关（逐标签取值），标签本身必须是 `<tag>` / `</tag>` 精确形态。
fn parse_ask(inner: &str) -> Option<Value> {
    // `<questions>` 包裹缺省容忍：直接按顶层 `<item>` 收集
    let section = element_inner(inner, "questions").unwrap_or_else(|| inner.to_string());
    let mut questions: Vec<Value> = Vec::new();
    let mut cursor = section;
    while let Some((item, rest)) = take_element(&cursor, "item") {
        if let Some(q) = parse_question(&item) {
            questions.push(q);
        }
        cursor = rest;
    }
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return None;
    }
    let mut args = Map::new();
    args.insert("questions".into(), Value::Array(questions));
    // 批准类语义（用户已拍板「完全等价」）：switchToAutoEdit 两种拼写都认
    if let Some(v) = bool_element(inner, "switchToAutoEdit")
        .or_else(|| bool_element(inner, "switch_to_auto_edit"))
    {
        args.insert("switchToAutoEdit".into(), json!(v));
    }
    // 显式方案正文（plan 档批准形的计划文件内容）；缺失/空白则不填，走既有回退
    if let Some(plan) = element_inner(inner, "plan").as_deref().and_then(clean_text) {
        args.insert("plan".into(), json!(plan));
    }
    Some(Value::Object(args))
}

/// 解析单题。先摘出 `<options>` 子树，避免题目级与选项级同名字段（`id`）互相污染。
fn parse_question(item: &str) -> Option<Value> {
    let (options_section, head) = match split_element(item, "options") {
        // 题目级字段可能分布在 `<options>` 两侧（实测载荷 id 在前、question 在后），
        // 故摘出选项子树后必须把前后缀拼回，否则会误读选项里的同名字段。
        Some((prefix, inner, suffix)) => (Some(inner), format!("{prefix}{suffix}")),
        None => (None, item.to_string()),
    };
    let id = clean_text(&element_inner(&head, "id")?)?;
    let question = clean_text(&element_inner(&head, "question")?)?;
    let mut q = Map::new();
    q.insert("id".into(), json!(id));
    q.insert("question".into(), json!(question));
    if let Some(section) = options_section {
        let mut options: Vec<Value> = Vec::new();
        let mut cursor = section;
        while let Some((opt, rest)) = take_element(&cursor, "item") {
            if let Some(o) = parse_option(&opt) {
                options.push(o);
            }
            cursor = rest;
        }
        if options.len() > MAX_OPTIONS {
            return None;
        }
        if !options.is_empty() {
            q.insert("options".into(), Value::Array(options));
        }
    }
    if let Some(single) = bool_element(&head, "single") {
        q.insert("single".into(), json!(single));
    }
    Some(Value::Object(q))
}

/// 解析单个选项：`id` + `label` 必填；`description` / `recommended` / `mode` 可选。
/// 非法 `mode` 值**丢弃**（而非整条放弃）——否则问答题本身也会一起没了。
fn parse_option(opt: &str) -> Option<Value> {
    let id = clean_text(&element_inner(opt, "id")?)?;
    let label = clean_text(&element_inner(opt, "label")?)?;
    let mut o = Map::new();
    o.insert("id".into(), json!(id));
    o.insert("label".into(), json!(label));
    if let Some(d) = element_inner(opt, "description")
        .as_deref()
        .and_then(clean_text)
    {
        o.insert("description".into(), json!(d));
    }
    if let Some(rec) = bool_element(opt, "recommended") {
        o.insert("recommended".into(), json!(rec));
    }
    if let Some(mode) = element_inner(opt, "mode")
        .as_deref()
        .and_then(clean_text)
        .and_then(|m| parse_mode(&m))
    {
        o.insert("mode".into(), json!(mode));
    }
    Some(Value::Object(o))
}

/// 取 `<tag>…</tag>` 的 (前缀, 内容, 后缀)。只认无属性的精确标签；同名嵌套按深度配对。
fn split_element(s: &str, tag: &str) -> Option<(String, String, String)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(open.as_str())?;
    let mut idx = start + open.len();
    let mut depth = 1usize;
    loop {
        let next_open = s[idx..].find(open.as_str()).map(|i| i + idx);
        let next_close = s[idx..].find(close.as_str()).map(|i| i + idx)?;
        if let Some(no) = next_open
            && no < next_close
        {
            depth += 1;
            idx = no + open.len();
            continue;
        }
        depth -= 1;
        if depth == 0 {
            return Some((
                s[..start].to_string(),
                s[start + open.len()..next_close].to_string(),
                s[next_close + close.len()..].to_string(),
            ));
        }
        idx = next_close + close.len();
    }
}

/// 取 `<tag>…</tag>` 的内容与「其后剩余文本」（顺序扫描多个同类元素时用）。
fn take_element(s: &str, tag: &str) -> Option<(String, String)> {
    split_element(s, tag).map(|(_, inner, suffix)| (inner, suffix))
}

/// 取 `<tag>` 的内容（不关心其后文本）。
fn element_inner(s: &str, tag: &str) -> Option<String> {
    take_element(s, tag).map(|(inner, _)| inner)
}

/// 布尔标签：`true|yes|1` → true，`false|no|0` → false，其余（含缺失）→ None。
fn bool_element(s: &str, tag: &str) -> Option<bool> {
    let raw = element_inner(s, tag)?;
    match scrub_control_tokens(&raw)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// 权限档位解析：走 `ApprovalMode` 自身的 serde 名（wire 即 snake_case），
/// 非法值返回 None —— 不硬编码档位清单，档位增删由枚举单点决定。
fn parse_mode(s: &str) -> Option<crate::core::prefs::ApprovalMode> {
    serde_json::from_value::<crate::core::prefs::ApprovalMode>(Value::String(s.to_string())).ok()
}

/// 字段值清洗：剥私有控制 token → 去首尾空白 → 空串视为缺省。
fn clean_text(v: &str) -> Option<String> {
    let t = scrub_control_tokens(v).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// 私有控制 token 清洗（实测 `]<]minimax[>[<`）：兼容端点会把这类 token 混进字段值，
/// 它们不是用户可见文案。只作用于**字段值**，不动正文其它部分。
///
/// 形态：`]?` + `<]` + name（≤32 个 `[A-Za-z0-9_.:-]`）+ `[` + `>` + `[?` + `<?`
/// —— 即 `<]name[>` 及其紧邻的 `]` / `[<` 残留；不匹配则原样保留。
fn scrub_control_tokens(v: &str) -> String {
    let chars: Vec<char> = v.chars().collect();
    let mut out = String::with_capacity(v.len());
    let mut i = 0usize;
    while i < chars.len() {
        // 先试「带前导 ]」的形态，避免把残留的 `]` 写出去
        if i + 2 < chars.len()
            && chars[i] == ']'
            && chars[i + 1] == '<'
            && chars[i + 2] == ']'
            && let Some(end) = control_token_end(&chars, i + 1)
        {
            i = end;
            continue;
        }
        if i + 1 < chars.len()
            && chars[i] == '<'
            && chars[i + 1] == ']'
            && let Some(end) = control_token_end(&chars, i)
        {
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `chars[i] == '<'` 且 `chars[i+1] == ']'` 时，返回 token 结束后的下标。
fn control_token_end(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 2;
    let name_start = j;
    while j < chars.len()
        && j - name_start < CONTROL_TOKEN_NAME_MAX
        && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '_' | '-' | '.' | ':'))
    {
        j += 1;
    }
    if j >= chars.len() || chars[j] != '[' {
        return None;
    }
    j += 1;
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    if j >= chars.len() || chars[j] != '>' {
        return None;
    }
    j += 1;
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    // 尾部残留 `[` / `<` **各自独立可选**消费：实测 token 形如 `]<]minimax[>[`
    //（紧随闭合标签出现；早先误读成 `[<` 的那个 `<` 其实是 `</question>` 的开头字节）
    // ——成对消费会剩 `[`、只消费 `[` 会剩 `<`，故两者分别判。
    if j < chars.len() && chars[j] == '[' {
        j += 1;
    }
    if j < chars.len() && chars[j] == '<' {
        j += 1;
    }
    Some(j)
}

/// 该字节偏移是否落在 Markdown 代码围栏内（``` / ~~~ 成对切换）。
fn inside_fence(text: &str, offset: usize) -> bool {
    let mut in_fence = false;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        if pos > offset {
            break;
        }
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
        }
        pos += line.len();
    }
    in_fence
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 会话 5da292d8（2026-09-25T03:29:29Z）的真实原文：模型把 ask 调用写成正文 XML，
    /// 且每个字段值尾部混入私有控制 token `]<]minimax[>[<`。
    const REAL_WORLD: &str = "我建议的下一步\n\n直接修代码的风险不大，但**得先确认你用的是哪一家**，否则改完也查不出额度。\n\n<ask>\n<questions>\n<item>\n<id>cn_domain_source</id>\n<options>\n<item>\n<description>你以为这是 MiniMax 官方域名。改 base_url 为 https://api.minimaxi.com/anthropic（中国）或 https://api.minimax.io/anthropic（国际）后重试]<]minimax[>[</description>\n<id>typo</id>\n<label>笔误、本来想填 minimaxi.com / minimax.io]<]minimax[>[</label>\n<recommended>true]<]minimax[>[</recommended>\n</item>\n<item>\n<description>你的 base_url 指的是某个公司/个人的反代服务，不是官方。这种情况属于代理生态问题，不适合在 CodeWave 里默认加白名单]<]minimax[>[</description>\n<id>proxy</id>\n<label>是中间层代理/自建网关]<]minimax[>[</label>\n</item>\n</options>\n<question>你使用的 api.minimax.cn 是什么来源？]<]minimax[>[</question>\n<single>true</single>\n</item>\n</questions>\n</ask>";

    #[test]
    fn salvages_real_world_payload() {
        let got = salvage_text_ask(REAL_WORLD).expect("实测原文必须被恢复");
        let q = &got.args["questions"][0];
        assert_eq!(q["id"], "cn_domain_source");
        assert_eq!(q["question"], "你使用的 api.minimax.cn 是什么来源？");
        assert_eq!(q["single"], true);
        let opts = q["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2, "两个选项都要保留：{opts:?}");
        assert_eq!(opts[0]["id"], "typo");
        assert_eq!(opts[0]["label"], "笔误、本来想填 minimaxi.com / minimax.io");
        assert_eq!(opts[0]["recommended"], true);
        assert!(opts[0]["description"].as_str().unwrap().ends_with("后重试"));
        assert!(
            !got.args.to_string().contains("minimax[>"),
            "控制 token 不得留在字段值里：{}",
            got.args
        );
        assert_eq!(opts[1]["id"], "proxy");
        assert!(opts[1].get("recommended").is_none(), "缺省 false 不填键");
        // 结构化通道（tools/ask）必须能直接吃下这份 args
        let _: crate::tools::ask::Args =
            serde_json::from_value(got.args.clone()).expect("必须是合法 ask 入参");
        // 块必须能被精确剥离，且保留块前的正文
        let stripped = strip_block(REAL_WORLD, &got.block);
        assert!(stripped.ends_with("否则改完也查不出额度。"));
        assert!(!stripped.contains("<ask>"));
    }

    #[test]
    fn maps_mode_switch_and_plan() {
        let text = "好\n\n<ask>\n<switchToAutoEdit>true</switchToAutoEdit>\n<plan>## 方案\n正文</plan>\n<questions>\n<item>\n<id>approve_plan</id>\n<question>批准吗？</question>\n<options>\n<item>\n<id>approve</id>\n<label>执行方案</label>\n<mode>full_access</mode>\n<recommended>true</recommended>\n</item>\n<item>\n<id>revise</id>\n<label>补充意见</label>\n<mode>不存在的档位</mode>\n</item>\n</options>\n</item>\n</questions>\n</ask>";
        let got = salvage_text_ask(text).expect("批准形必须被恢复");
        assert_eq!(got.args["switchToAutoEdit"], true);
        assert!(got.args["plan"].as_str().unwrap().contains("## 方案"));
        let opts = got.args["questions"][0]["options"].as_array().unwrap();
        assert_eq!(opts[0]["mode"], "full_access");
        assert!(
            opts[1].get("mode").is_none(),
            "非法 mode 丢弃而非整条放弃：{opts:?}"
        );
        // 档位名走 ApprovalMode 自身的 serde 名：能反序列化 + 与 schema 枚举一致
        assert!(
            serde_json::from_value::<crate::core::prefs::ApprovalMode>(json!("full_access"))
                .is_ok()
        );
        // 与真实工具调用同形：结构化通道整份吃下
        let _: crate::tools::ask::Args = serde_json::from_value(got.args.clone()).unwrap();
    }

    #[test]
    fn rejects_ambiguous_shapes() {
        // 未闭合
        let unclosed =
            "看看\n<ask><questions><item><id>a</id><question>b</question></item></questions>";
        assert!(salvage_text_ask(unclosed).is_none());
        // 块后仍有正文（讲解协议的典型形态）
        let tail = "示例：<ask><questions><item><id>a</id><question>b</question></item></questions></ask> 以上是格式。";
        assert!(salvage_text_ask(tail).is_none());
        // 代码围栏内
        let fenced = "格式如下：\n```\n<ask><questions><item><id>a</id><question>b</question></item></questions></ask>\n```\n";
        assert!(salvage_text_ask(fenced).is_none());
        // 缺 question
        assert!(
            salvage_text_ask("<ask><questions><item><id>a</id></item></questions></ask>").is_none()
        );
        // 空题
        assert!(salvage_text_ask("<ask><questions></questions></ask>").is_none());
        // 超 5 题
        let many: String = (0..6)
            .map(|i| format!("<item><id>q{i}</id><question>问{i}</question></item>"))
            .collect();
        assert!(salvage_text_ask(&format!("<ask><questions>{many}</questions></ask>")).is_none());
        // 单题超 6 选项
        let many_opts: String = (0..7)
            .map(|i| format!("<item><id>o{i}</id><label>选{i}</label></item>"))
            .collect();
        let one = format!(
            "<ask><questions><item><id>q</id><question>问</question><options>{many_opts}</options></item></questions></ask>"
        );
        assert!(salvage_text_ask(&one).is_none());
        // 无 ask 块
        assert!(salvage_text_ask("就是普通回答").is_none());
    }

    #[test]
    fn tolerates_field_order_and_bool_spellings() {
        // options 在 id 之前（弱模型会这么排）：摘出 options 子树后仍能读到题目 id
        let text = "<ask><questions><item><options><item><id>o1</id><label>选一</label></item></options><id>q1</id><question>问一</question></item></questions></ask>";
        let got = salvage_text_ask(text).expect("字段顺序无关");
        assert_eq!(got.args["questions"][0]["id"], "q1");
        assert_eq!(got.args["questions"][0]["options"][0]["id"], "o1");
        // yes 也算 true
        let text2 = "<ask><questions><item><id>q</id><question>问</question><options><item><id>o</id><label>选</label><recommended>yes</recommended></item></options></item></questions></ask>";
        let got2 = salvage_text_ask(text2).unwrap();
        assert_eq!(got2.args["questions"][0]["options"][0]["recommended"], true);
    }

    #[test]
    fn scrub_removes_private_tokens_only() {
        assert_eq!(scrub_control_tokens("后重试]<]minimax[>[<"), "后重试");
        assert_eq!(
            scrub_control_tokens("正常文案（含方括号 [x] 与 <tag>）"),
            "正常文案（含方括号 [x] 与 <tag>）"
        );
        assert_eq!(scrub_control_tokens("<]a[>"), "");
    }

    #[test]
    fn strip_block_keeps_prose_and_trims_tail() {
        let block =
            "<ask><questions><item><id>a</id><question>b</question></item></questions></ask>";
        let text = format!("前面的话\n\n{block}\n\n");
        assert_eq!(strip_block(&text, block), "前面的话");
        // 块后还有正文时不剥离（调用方不会走到这里，纯函数保持最小惊讶）
        let mid = format!("前\n{block}\n后");
        assert!(strip_block(&mid, block).contains("后"));
    }

    #[test]
    fn drops_unanswerable_items_and_keeps_the_rest() {
        // 应答协议按 id 关联（answers[qid].selections）：缺 id/question 的题、缺 id/label 的选项
        // 本来就答不了 → 逐项丢弃；能答的部分照常呈现（整条放弃则用户什么都看不到）。
        let text = r#"<ask><questions>
<item><id>q1</id><question>能答的问</question><options><item><id>o1</id><label>能答的选项</label></item><item><label>缺 id 的选项</label></item></options></item>
<item><question>缺 id 的题</question></item>
</questions></ask>"#;
        let got = salvage_text_ask(text).expect("可答的部分必须留住");
        let qs = got.args["questions"].as_array().unwrap();
        assert_eq!(qs.len(), 1, "缺 id 的题被丢弃：{qs:?}");
        assert_eq!(qs[0]["id"], "q1");
        let opts = qs[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 1, "缺 id 的选项被丢弃：{opts:?}");
        assert_eq!(opts[0]["id"], "o1");
    }

    #[test]
    fn requires_block_on_its_own_starting_line() {
        // 行内引述 / 行内代码里的完整示例不触发
        let inline = r#"格式是 <ask><questions><item><id>a</id><question>b</question></item></questions></ask>"#;
        assert!(salvage_text_ask(inline).is_none());
        // 行首即块（无前导换行）仍认
        let head =
            r#"<ask><questions><item><id>a</id><question>b</question></item></questions></ask>"#;
        assert!(salvage_text_ask(head).is_some());
        // 另起一行也认
        let after_nl = r#"前面的话
<ask><questions><item><id>a</id><question>b</question></item></questions></ask>"#;
        assert!(salvage_text_ask(after_nl).is_some());
    }

    #[test]
    fn rejects_partial_tag_names_and_multiple_blocks() {
        // 标签名必须精确（<asking> 不算）
        assert!(salvage_text_ask(r#"<asking>x</asking>"#).is_none());
        // 多块并存 = 歧义
        let two = r#"<ask><questions><item><id>a</id><question>b</question></item></questions><ask><questions><item><id>c</id><question>d</question></item></questions></ask>"#;
        assert!(salvage_text_ask(two).is_none());
    }

    #[test]
    fn salvage_allowed_after_a_closed_fence() {
        // 围栏成对收敛后，后面的块照常认（只钉「围栏内拒绝」会漏掉 toggle 不收的回归）
        let text = r#"```
示例
```

<ask><questions><item><id>a</id><question>b</question></item></questions></ask>"#;
        assert!(salvage_text_ask(text).is_some());
    }

    #[test]
    fn stripping_the_whole_text_leaves_empty() {
        let block =
            r#"<ask><questions><item><id>a</id><question>b</question></item></questions></ask>"#;
        assert_eq!(strip_block(block, block), "");
    }
}
