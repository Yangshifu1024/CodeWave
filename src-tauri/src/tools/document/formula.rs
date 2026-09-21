//! 公式求值（白名单）：判断一个公式能不能自己算出值，能算就把值一并写进缓存。
//!
//! ## 为什么只做白名单
//!
//! 需求里写的是「简单的自己算好填上；复杂的打『打开时重算』标记」。**正确性优先于覆盖面**：
//! 缓存值是别的工具（以及 Excel 之外的读取方）唯一能看到的东西，写错一个数字比不写数字
//! 糟糕得多——写错的数没有任何迹象，使用者会拿它当真。所以这里只认一小块确定无疑的语法：
//!
//! - 纯数值四则运算（`+ - * /`、括号、一元负号）
//! - `SUM` / `AVERAGE` / `COUNT` / `MIN` / `MAX`，参数是单元格或单元格区域
//!
//! 其余（区域之外的函数、跨工作表、带 `$` 以外的奇怪写法、定义名称、数组公式……）
//! 一律判定为「算不出来」，交给 Excel 打开时重算。
//!
//! ## 两条保守规则
//!
//! 1. **引用的单元格里还有公式就不算**：公式的缓存值可能过期，拿它去算等于把错误传播出去。
//! 2. **算出来的不是有限数就不算**：除零、溢出、结果 NaN/Infinity 在 Excel 里是错误值，
//!    错误值不能当成数字写进 `<v>`。

use super::sheet_edit::CellSnapshot;

/// 区域展开的单元格数上限：防止一句 `SUM(A1:XFD1048576)` 把内存吃光。
const MAX_RANGE_CELLS: u64 = 100_000;
/// 表达式嵌套深度上限：防止构造出来的深层括号把栈吃光。
const MAX_DEPTH: u32 = 32;

/// 一个单元格坐标：工作表名（None = 当前工作表）+ 列号 + 行号。
type CellRef = (Option<String>, u32, u32);

/// 求值时的取值来源：给坐标，回答那个单元格里是什么。
/// `None` = 读不出来（引用了不存在的工作表、工作表内容没载入），遇到就放弃求值。
pub type Lookup<'a> = &'a dyn Fn(Option<&str>, u32, u32) -> Option<CellSnapshot>;

/// 求值结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Computed {
    /// 算出来了，值就是它。
    Value(f64),
    /// 算不出来（语法不在白名单内、引用了公式、结果不是有限数……）。
    /// 调用方据此给工作簿打「打开时重算」标记。
    NeedsRecalc,
}

/// 收集公式引用到的工作表名（不含当前工作表的引用）。
///
/// 调用方用它决定还要额外载入哪些工作表：跨表引用（`=SUM(明细!B2:B3)`）要能读到那张表的
/// 单元格，否则只能判「算不出来」。
pub fn referenced_sheets(formula: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = formula.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '!' {
            // 往回取工作表名：`'月度 汇总'!` 与 `明细!` 两种写法
            let mut j = i;
            let name = if j > 0 && chars[j - 1] == '\'' {
                let mut k = j - 1;
                while k > 0 && chars[k - 1] != '\'' {
                    k -= 1;
                }
                let s: String = chars[k..j - 1].iter().collect();
                i = j + 1;
                let _ = &mut j;
                s
            } else {
                let mut k = j;
                while k > 0 && (chars[k - 1].is_alphanumeric() || chars[k - 1] == '_') {
                    k -= 1;
                }
                let s: String = chars[k..j].iter().collect();
                s
            };
            // 两端各去一个引号（Excel 里单引号是靠写两个单引号转义的）
            let cleaned = name.replace("''", "'");
            if !cleaned.is_empty() && !out.contains(&cleaned) {
                out.push(cleaned);
            }
        }
        i += 1;
    }
    out
}

/// 给一个公式估值。
pub fn evaluate(formula: &str, lookup: Lookup<'_>) -> Computed {
    let text = formula.trim().trim_start_matches('=').trim();
    if text.is_empty() {
        return Computed::NeedsRecalc;
    }
    let mut p = Parser {
        chars: text.chars().collect(),
        at: 0,
        lookup,
        depth: 0,
    };
    let v = p.expr();
    p.skip_ws();
    // 尾巴上还有东西说明没解析完（例如多了个 `&`），不算
    if p.at != p.chars.len() {
        return Computed::NeedsRecalc;
    }
    match v {
        Some(n) if n.is_finite() => Computed::Value(n),
        _ => Computed::NeedsRecalc,
    }
}

/// 递归下降解析器；任何一步不认得就返回 None，由调用方判定「打开时重算」。
struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    lookup: Lookup<'a>,
    depth: u32,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.at += 1;
        }
    }

    /// 吃掉一个字面量字符（不匹配则回退）。
    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// 表达式：加减。
    fn expr(&mut self) -> Option<f64> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let mut acc = self.term()?;
        loop {
            self.skip_ws();
            let op = match self.peek() {
                Some('+') => '+',
                Some('-') => '-',
                _ => break,
            };
            self.at += 1;
            let rhs = self.term()?;
            acc = if op == '+' { acc + rhs } else { acc - rhs };
        }
        self.depth -= 1;
        if acc.is_finite() { Some(acc) } else { None }
    }

    /// 项：乘除。
    fn term(&mut self) -> Option<f64> {
        let mut acc = self.unary()?;
        loop {
            self.skip_ws();
            let op = match self.peek() {
                Some('*') => '*',
                Some('/') => '/',
                _ => break,
            };
            self.at += 1;
            let rhs = self.unary()?;
            if op == '/' && rhs == 0.0 {
                // 除零在 Excel 里是 #DIV/0!，不是 0，也不是无穷——不猜
                return None;
            }
            acc = if op == '*' { acc * rhs } else { acc / rhs };
        }
        if acc.is_finite() { Some(acc) } else { None }
    }

    /// 一元：负号、括号、函数调用、数字、单元格引用。
    fn unary(&mut self) -> Option<f64> {
        self.skip_ws();
        if self.peek() == Some('-') {
            self.at += 1;
            return self.unary().map(|v| -v);
        }
        if self.peek() == Some('+') {
            self.at += 1;
            return self.unary();
        }
        if self.peek() == Some('(') {
            self.at += 1;
            let v = self.expr()?;
            if !self.eat(')') {
                return None;
            }
            return Some(v);
        }
        if matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            return self.number();
        }
        if matches!(self.peek(), Some(c) if c.is_alphabetic() || c == '\'' || c == '_') {
            // 先按坐标试探：`B3` 与 `SUM(...)` 都从字母开头
            let save = self.at;
            if let Some(v) = self.cell_or_range_value() {
                return Some(v);
            }
            self.at = save;
            return self.func();
        }
        None
    }

    /// 单元格引用直接参与运算：`=A1+B1`。区域不合法（区域只能当函数参数）。
    fn cell_or_range_value(&mut self) -> Option<f64> {
        let save = self.at;
        let (sheet, col, row) = self.cell_ref()?;
        self.skip_ws();
        if self.peek() == Some(':') {
            self.at = save;
            return None;
        }
        match (self.lookup)(sheet.as_deref(), col, row) {
            Some(CellSnapshot::Number(n)) => Some(n),
            // 文本、空、公式参与算术都不猜（Excel 会给 #VALUE!）
            Some(CellSnapshot::Text) | Some(CellSnapshot::Empty) => {
                self.at = save;
                None
            }
            Some(CellSnapshot::Formula) | None => {
                self.at = save;
                None
            }
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.at;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            self.at += 1;
        }
        // 科学计数法（1e3）一并认下，Excel 里也这么写
        if matches!(self.peek(), Some('e') | Some('E')) {
            let save = self.at;
            self.at += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.at += 1;
            }
            let digit_start = self.at;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.at += 1;
            }
            if self.at == digit_start {
                self.at = save;
            }
        }
        self.chars[start..self.at]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }

    /// 函数调用：白名单之外一律不算。
    fn func(&mut self) -> Option<f64> {
        let start = self.at;
        while matches!(self.peek(), Some(c) if c.is_alphanumeric() || c == '_') {
            self.at += 1;
        }
        let name: String = self.chars[start..self.at].iter().collect();
        let upper = name.to_ascii_uppercase();
        if !matches!(upper.as_str(), "SUM" | "AVERAGE" | "COUNT" | "MIN" | "MAX") {
            return None;
        }
        if !self.eat('(') {
            return None;
        }
        let mut nums: Vec<f64> = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(')') {
                break;
            }
            // 参数只接受单元格或单元格区域——需求里写的就是这一条
            let (vals, is_range) = self.arg_numbers()?;
            if !is_range && vals.len() != 1 {
                return None;
            }
            nums.extend(vals);
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            if self.eat(')') {
                break;
            }
            return None;
        }
        match upper.as_str() {
            "SUM" => Some(nums.iter().sum()),
            "COUNT" => Some(nums.len() as f64),
            "MIN" => nums.iter().copied().reduce(f64::min),
            "MAX" => nums.iter().copied().reduce(f64::max),
            // AVERAGE 对空集合是 #DIV/0!，不猜
            "AVERAGE" => {
                if nums.is_empty() {
                    None
                } else {
                    Some(nums.iter().sum::<f64>() / nums.len() as f64)
                }
            }
            _ => None,
        }
    }

    /// 一个参数：单元格或区域。
    /// 返回（区域里**数值型**单元格的值，是否区域）。
    /// 文本与空格按 Excel 的惯例忽略（SUM/COUNT 都不数它们）；遇到公式一律放弃。
    fn arg_numbers(&mut self) -> Option<(Vec<f64>, bool)> {
        let (sheet, c1, r1) = self.cell_ref()?;
        self.skip_ws();
        if !self.eat(':') {
            return Some((self.one(sheet.as_deref(), c1, r1)?, false));
        }
        let (sheet2, c2, r2) = self.cell_ref()?;
        // 区域端点常常省略表名（`明细!A1:A9` 的第二个端点）——默认沿用起点那张表。
        // 两张不同的表用冒号连起来不是合法写法，不猜。
        let sheet = match (&sheet, &sheet2) {
            (Some(a), Some(b)) if a != b => return None,
            (Some(a), _) => Some(a.clone()),
            (None, b) => b.clone(),
        };
        let (r_lo, r_hi) = (r1.min(r2), r1.max(r2));
        let (c_lo, c_hi) = (c1.min(c2), c1.max(c2));
        let count = (r_hi as u64 - r_lo as u64 + 1) * (c_hi as u64 - c_lo as u64 + 1);
        if count > MAX_RANGE_CELLS {
            return None;
        }
        let mut out = Vec::new();
        for r in r_lo..=r_hi {
            for c in c_lo..=c_hi {
                match (self.lookup)(sheet.as_deref(), c, r)? {
                    CellSnapshot::Number(n) => out.push(n),
                    CellSnapshot::Text | CellSnapshot::Empty => {}
                    CellSnapshot::Formula => return None,
                }
            }
        }
        Some((out, true))
    }

    /// 单个单元格：只有数值型才算得出；公式与读不出来的都放弃。
    fn one(&self, sheet: Option<&str>, col: u32, row: u32) -> Option<Vec<f64>> {
        match (self.lookup)(sheet, col, row)? {
            CellSnapshot::Number(n) => Some(vec![n]),
            // 单个非数值单元格参与算术在 Excel 里是 #VALUE!，不猜
            CellSnapshot::Text | CellSnapshot::Empty | CellSnapshot::Formula => None,
        }
    }

    /// 单元格引用：`B3`、`$B$3`、`明细!B3`、`'月度 汇总'!$B$3`。
    fn cell_ref(&mut self) -> Option<(Option<String>, u32, u32)> {
        self.skip_ws();
        let mut sheet: Option<String> = None;
        let save = self.at;
        if self.peek() == Some('\'') {
            self.at += 1;
            let mut name = String::new();
            loop {
                match self.peek() {
                    Some('\'') => {
                        self.at += 1;
                        if self.peek() == Some('\'') {
                            name.push('\'');
                            self.at += 1;
                        } else {
                            break;
                        }
                    }
                    Some(c) => {
                        name.push(c);
                        self.at += 1;
                    }
                    None => return None,
                }
            }
            if !self.eat('!') {
                self.at = save;
                return None;
            }
            sheet = Some(name);
        } else {
            // 未加引号的工作表名：字母/数字/下划线/中日文，后面必须紧跟 `!`
            let mut k = self.at;
            while matches!(self.chars.get(k), Some(c) if c.is_alphanumeric() || *c == '_') {
                k += 1;
            }
            if k < self.chars.len() && self.chars[k] == '!' && k > self.at {
                sheet = Some(self.chars[self.at..k].iter().collect());
                self.at = k + 1;
            }
        }

        let mut col: u32 = 0;
        let mut letters = 0usize;
        while let Some(c) = self.peek() {
            if c == '$' {
                self.at += 1;
                continue;
            }
            if c.is_ascii_alphabetic() && letters < 3 {
                col = col * 26 + (c.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
                letters += 1;
                self.at += 1;
            } else {
                break;
            }
        }
        if letters == 0 || col == 0 || col > super::xlsx::MAX_COLS {
            self.at = save;
            return None;
        }
        let mut row: u32 = 0;
        let mut digits = 0usize;
        while let Some(c) = self.peek() {
            if c == '$' {
                self.at += 1;
                continue;
            }
            if c.is_ascii_digit() && digits < 7 {
                row = row * 10 + (c as u32 - '0' as u32);
                digits += 1;
                self.at += 1;
            } else {
                break;
            }
        }
        if digits == 0 || row == 0 || row > super::xlsx::MAX_ROWS {
            self.at = save;
            return None;
        }
        Some((sheet, col, row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一张表：A1=1、A2=2、A3=3、B1=文本、B2=空、C1=公式。
    fn lookup(sheet: Option<&str>, col: u32, row: u32) -> Option<CellSnapshot> {
        if sheet == Some("别的表") {
            return Some(CellSnapshot::Number(10.0));
        }
        if sheet == Some("不存在") {
            return None;
        }
        Some(match (col, row) {
            (1, 1) => CellSnapshot::Number(1.0),
            (1, 2) => CellSnapshot::Number(2.0),
            (1, 3) => CellSnapshot::Number(3.0),
            (2, 1) => CellSnapshot::Text,
            (3, 1) => CellSnapshot::Formula,
            _ => CellSnapshot::Empty,
        })
    }

    fn value(f: &str) -> Option<f64> {
        match evaluate(f, &lookup) {
            Computed::Value(n) => Some(n),
            Computed::NeedsRecalc => None,
        }
    }

    #[test]
    fn arithmetic_and_parentheses() {
        assert_eq!(value("=1+2*3"), Some(7.0));
        assert_eq!(value("=(1+2)*3"), Some(9.0));
        assert_eq!(value("=10/4"), Some(2.5));
        assert_eq!(value("=-3+1"), Some(-2.0));
        assert_eq!(value("=2.5e2"), Some(250.0));
        // 不带前导等号也认（模型有时会漏）
        assert_eq!(value("1+1"), Some(2.0));
    }

    #[test]
    fn whitelisted_functions() {
        assert_eq!(value("=SUM(A1:A3)"), Some(6.0));
        assert_eq!(value("=AVERAGE(A1:A3)"), Some(2.0));
        assert_eq!(value("=COUNT(A1:B2)"), Some(2.0), "文本与空格不数");
        assert_eq!(value("=MIN(A1:A3)"), Some(1.0));
        assert_eq!(value("=MAX(A1:A3)"), Some(3.0));
        // 大小写不影响
        assert_eq!(value("=sum(a1:a3)"), Some(6.0));
        // 多参数
        assert_eq!(
            value("=SUM(A1,A2,10)"),
            None,
            "数字字面量不算「单元格区域」参数"
        );
        assert_eq!(value("=SUM(A1:A2,A3)"), Some(6.0));
    }

    #[test]
    fn cross_sheet_reference_is_supported() {
        assert_eq!(value("=SUM(别的表!A1:A2)"), Some(20.0));
        assert_eq!(value("='别的表'!A1+5"), Some(15.0));
        assert_eq!(
            referenced_sheets("=SUM(明细!B2:B3)+'月度 汇总'!C1"),
            vec!["明细".to_string(), "月度 汇总".to_string()]
        );
        assert!(referenced_sheets("=SUM(A1:A3)").is_empty());
    }

    #[test]
    fn formulas_and_errors_are_refused() {
        // 引用的单元格里还有公式：缓存值可能过期，拿它算等于传播错误
        assert_eq!(value("=C1+1"), None);
        assert_eq!(value("=SUM(A1:C1)"), None, "区域里混进公式也不算");
        // 除零是 #DIV/0!，不是 0 也不是无穷
        assert_eq!(value("=1/0"), None);
        // 空单元格参与算术是 #VALUE!
        assert_eq!(value("=B2+1"), None);
        // 文本单元格同理
        assert_eq!(value("=B1+1"), None);
        // 引用不存在的工作表：读不出来 → 不算（Excel 此时是 #REF!）
        assert_eq!(value("=SUM(不存在!A1:A2)"), None);
        // 白名单之外的函数不猜
        assert_eq!(value("=VLOOKUP(1,A1:B3,2)"), None);
        assert_eq!(value("=IF(A1>0,1,2)"), None);
        // 拼接、比较、区域运算一律不认
        assert_eq!(value("=A1&A2"), None);
        assert_eq!(value("=A1>A2"), None);
        assert_eq!(value("=A1:A3"), None);
        assert_eq!(value("=SUM(A1:A3)+"), None, "残缺表达式不猜");
        assert_eq!(value(""), None);
    }

    #[test]
    fn only_numbers_count_inside_ranges() {
        // A1:A3 全数字、B 列是文本/空 → 只看数值
        assert_eq!(value("=SUM(A1:B3)"), Some(6.0));
        assert_eq!(value("=MIN(A1:B3)"), Some(1.0));
    }

    #[test]
    fn huge_ranges_are_refused_not_exploded() {
        // 整列整行这种写法展开起来是天文数字，直接判「算不出来」
        assert_eq!(value("=SUM(A1:XFD1048576)"), None);
    }

    #[test]
    fn average_of_empty_set_is_refused() {
        // 空区域求平均在 Excel 里是 #DIV/0!
        assert_eq!(value("=AVERAGE(D1:D9)"), None);
        assert_eq!(value("=SUM(D1:D9)"), Some(0.0), "求和空区域是 0");
    }

    #[test]
    fn deep_nesting_does_not_blow_the_stack() {
        let deep = format!("={}1{}", "(".repeat(200), ")".repeat(200));
        assert_eq!(value(&deep), None);
    }
}
