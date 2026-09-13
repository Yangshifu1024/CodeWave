//! calculate 工具：手写递归下降数学求值器（[docs/p1-plan](../../../docs/p1-plan.md) §2.8）。
//! 文法：expr(加减) → term(乘除模) → factor(^ 右结合) → unary(−) → atom。
//! 永不 eval；除零 / 溢出 / NaN 均产生干净错误。

use serde_json::{json, Value};

/// 求值一个数学表达式字符串；非法输入返回干净的错误信息。
pub fn evaluate(input: &str) -> Result<f64, String> {
    let tokens = tokenize(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let v = p.parse_expr()?;
    if p.pos != p.tokens.len() {
        return Err(format!("表达式在位置 {} 后有多余内容", p.pos));
    }
    if !v.is_finite() {
        return Err("结果不是有限数（溢出或 NaN）".into());
    }
    Ok(v)
}

/// 数学记号：数字 / 标识符（函数名或常量名）/ 运算符 / 括号 / 逗号。
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// 数字字面量。
    Num(f64),
    /// 标识符（常量或函数名）。
    Ident(String),
    /// 运算符（+ - * / % ^）。
    Op(char),
    /// 左括号。
    LParen,
    /// 右括号。
    RParen,
    /// 参数分隔逗号。
    Comma,
}

/// 词法分析：把表达式字符串切分为记号序列；非法字符 / 多小数点即报错。
fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                let mut seen_dot = false;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    if chars[i] == '.' {
                        if seen_dot {
                            return Err("数字中包含多个小数点".into());
                        }
                        seen_dot = true;
                    }
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let n: f64 = text.parse().map_err(|_| format!("非法数字：{text}"))?;
                out.push(Tok::Num(n));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            '+' | '-' | '*' | '/' | '%' | '^' => {
                out.push(Tok::Op(c));
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            other => return Err(format!("非法字符：{other:?}")),
        }
    }
    if out.is_empty() {
        return Err("表达式为空".into());
    }
    Ok(out)
}

/// 递归下降解析器：持有记号序列与当前位置。
struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    /// 前瞻当前记号（不消费）。
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    /// 消费并返回当前记号。
    fn next(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// expr 层：加减（最低优先级，左结合）。
    fn parse_expr(&mut self) -> Result<f64, String> {
        let mut left = self.parse_term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek() {
            let op = *op;
            self.pos += 1;
            let right = self.parse_term()?;
            left = match op {
                '+' => left + right,
                _ => left - right,
            };
        }
        Ok(left)
    }

    /// term 层：乘除模（除零 / 模零显式报错）。
    fn parse_term(&mut self) -> Result<f64, String> {
        let mut left = self.parse_factor()?;
        while let Some(Tok::Op(op @ ('*' | '/' | '%'))) = self.peek() {
            let op = *op;
            self.pos += 1;
            let right = self.parse_factor()?;
            left = match op {
                '*' => left * right,
                '/' => {
                    if right == 0.0 {
                        return Err("除以零".into());
                    }
                    left / right
                }
                _ => {
                    if right == 0.0 {
                        return Err("模零".into());
                    }
                    left % right
                }
            };
        }
        Ok(left)
    }

    /// factor 层：一元正负号绑定比 ^ 松（-2^2 = -(2^2)）；指数侧允许一元负号（2^-1）。
    fn parse_factor(&mut self) -> Result<f64, String> {
        if let Some(Tok::Op('-')) = self.peek() {
            self.pos += 1;
            return Ok(-self.parse_factor()?);
        }
        if let Some(Tok::Op('+')) = self.peek() {
            self.pos += 1;
            return self.parse_factor();
        }
        let base = self.parse_atom()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            let exp = self.parse_factor()?; // 右结合
            let v = base.powf(exp);
            if !v.is_finite() {
                return Err("幂运算溢出".into());
            }
            return Ok(v);
        }
        Ok(base)
    }

    /// atom 层：数字 / 括号子表达式 / 常量（pi e tau）或函数调用。
    fn parse_atom(&mut self) -> Result<f64, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::LParen) => {
                let v = self.parse_expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(v),
                    _ => Err("缺少右括号".into()),
                }
            }
            Some(Tok::Ident(name)) => {
                let lname = name.to_ascii_lowercase();
                let const_v = match lname.as_str() {
                    "pi" => std::f64::consts::PI,
                    "e" => std::f64::consts::E,
                    "tau" => std::f64::consts::TAU,
                    _ => {
                        return self.call_function(&name);
                    }
                };
                Ok(const_v)
            }
            Some(Tok::Op(op)) => Err(format!("意外的运算符 {op:?}")),
            Some(other) => Err(format!("意外的记号 {other:?}")),
            None => Err("表达式意外结束".into()),
        }
    }

    /// 解析函数调用（参数列表逗号分隔，允许零参）。
    fn call_function(&mut self, name: &str) -> Result<f64, String> {
        let lname = name.to_ascii_lowercase();
        match self.next() {
            Some(Tok::LParen) => {}
            _ => return Err(format!("函数 {name} 后需要括号")),
        }
        let mut args = Vec::new();
        if !matches!(self.peek(), Some(Tok::RParen)) {
            loop {
                args.push(self.parse_expr()?);
                match self.next() {
                    Some(Tok::Comma) => continue,
                    Some(Tok::RParen) => break,
                    _ => return Err(format!("函数 {name} 参数列表格式错误")),
                }
            }
        } else {
            self.pos += 1; // 消费零参调用的右括号
        }
        apply_function(&lname, &args, name)
    }
}

/// 按函数名分发计算（参数个数校验 + 结果有限性检查）。
fn apply_function(lname: &str, args: &[f64], name: &str) -> Result<f64, String> {
    let arg = |i: usize| -> Result<f64, String> {
        args.get(i)
            .copied()
            .ok_or_else(|| format!("{} 需要第 {} 个参数", name, i + 1))
    };
    let one_arg = |args: &[f64], name: &str| -> Result<f64, String> {
        if args.len() != 1 {
            return Err(format!("{name} 需要 1 个参数"));
        }
        Ok(args[0])
    };
    let two_args = |args: &[f64], name: &str| -> Result<(f64, f64), String> {
        if args.len() != 2 {
            return Err(format!("{name} 需要 2 个参数"));
        }
        Ok((args[0], args[1]))
    };
    let v = match lname {
        "sqrt" => one_arg(args, name)?.sqrt(),
        "cbrt" => one_arg(args, name)?.cbrt(),
        "ln" => one_arg(args, name)?.ln(),
        "log" => {
            let (a, b) = two_args(args, name)?;
            a.log(b)
        }
        "log2" => one_arg(args, name)?.log2(),
        "log10" => one_arg(args, name)?.log10(),
        "exp" => one_arg(args, name)?.exp(),
        "abs" => one_arg(args, name)?.abs(),
        "floor" => one_arg(args, name)?.floor(),
        "ceil" => one_arg(args, name)?.ceil(),
        "round" => one_arg(args, name)?.round(),
        "sin" => one_arg(args, name)?.sin(),
        "cos" => one_arg(args, name)?.cos(),
        "tan" => one_arg(args, name)?.tan(),
        "asin" => one_arg(args, name)?.asin(),
        "acos" => one_arg(args, name)?.acos(),
        "atan" => one_arg(args, name)?.atan(),
        "atan2" => {
            let (a, b) = two_args(args, name)?;
            a.atan2(b)
        }
        "min" => args.iter().copied().fold(f64::INFINITY, f64::min),
        "max" => args.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "pow" => {
            let (a, b) = two_args(args, name)?;
            a.powf(b)
        }
        _ => return Err(format!("未知函数：{name}（可用：sqrt cbrt ln log log2 log10 exp abs floor ceil round sin cos tan asin acos atan atan2 min max pow；常量 pi e tau）")),
    };
    let _ = arg;
    if !v.is_finite() {
        return Err(format!("{name} 结果溢出或未定义"));
    }
    Ok(v)
}

/// calculate 工具：纯 Rust 数学表达式求值，不经 shell、永不 eval。
pub struct CalculateTool;

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};

#[async_trait::async_trait]
impl Tool for CalculateTool {
    fn name(&self) -> &'static str {
        "calculate"
    }
    fn description(&self) -> &'static str {
        "精确求值数学表达式（不经 shell）。运算符：+ - * / % ^（右结合）；函数：sqrt cbrt ln log(a,b) log2 log10 exp abs floor ceil round sin cos tan asin acos atan atan2 min max pow；常量：pi e tau。示例：sqrt(144) + 2^3"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["expression"],
  "properties": {
    "expression": {"type": "string", "description": "数学表达式"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn run(&self, _ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let expr = match args["expression"].as_str() {
            Some(e) => e,
            None => return ToolOutcome::err("E_ARGS", "缺少 expression 参数"),
        };
        match evaluate(expr) {
            Ok(v) => {
                let text = format!("{v:.12}")
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string();
                ToolOutcome::ok(json!({ "expression": expr, "result": text }))
            }
            Err(e) => ToolOutcome::err("E_CALC", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn basics() {
        assert!(close(evaluate("1+2*3").unwrap(), 7.0));
        assert!(close(evaluate("(1+2)*3").unwrap(), 9.0));
        assert!(close(evaluate("2^10").unwrap(), 1024.0));
        assert!(close(evaluate("-3 + 5").unwrap(), 2.0));
        assert!(close(evaluate("10 % 3").unwrap(), 1.0));
        assert!(close(evaluate("sqrt(144) + 2^3").unwrap(), 20.0));
        assert!(close(evaluate("pi").unwrap(), std::f64::consts::PI));
        assert!(close(evaluate("log(8, 2)").unwrap(), 3.0));
        assert!(close(evaluate("min(3, 1, 2)").unwrap(), 1.0));
        assert!(close(evaluate("max(3, 1, 2)").unwrap(), 3.0));
        assert!(close(
            evaluate("atan2(1, 1)").unwrap(),
            std::f64::consts::FRAC_PI_4
        ));
    }

    #[test]
    fn right_assoc_power_and_unary() {
        assert!(close(evaluate("2^3^2").unwrap(), 512.0)); // 2^(3^2)，右结合
        assert!(close(evaluate("-2^2").unwrap(), -4.0)); // -(2^2)
        assert!(close(evaluate("2^-1").unwrap(), 0.5));
    }

    #[test]
    fn errors_are_clean() {
        assert!(evaluate("1/0").is_err());
        assert!(evaluate("1%0").is_err());
        assert!(evaluate("1..2").is_err());
        assert!(evaluate("(1+2").is_err());
        assert!(evaluate("foo(1)").is_err());
        assert!(evaluate("1 2").is_err());
        assert!(evaluate("").is_err());
        assert!(evaluate("sqrt(1,2)").is_err());
        assert!(evaluate("10^10^10").is_err()); // 溢出
    }
}
