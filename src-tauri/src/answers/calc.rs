//! Calculator for quick answers: a small recursive-descent parser.

use super::number::parse_number;

/// A successful calculation: the value and the normalised expression for display.
#[derive(Debug, Clone, PartialEq)]
pub struct Calc {
    pub value: f64,
    pub shown: String,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Percent,
    LParen,
    RParen,
    Of,
}

const MAX_DEPTH: usize = 64;

fn tokenize(text: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = text.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let is_num = |c: char| c.is_ascii_digit() || c == '.' || c == ',';
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if is_num(c) {
            let start = i;
            while i < chars.len() && is_num(chars[i]) {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            let mut v = parse_number(&s)?;
            // Scientific notation: `2e3`, `1,5e-3` (no spaces) is one number literal.
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                    j += 1;
                }
                let digits = j;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j > digits {
                    let exp: String = chars[i + 1..j].iter().collect();
                    let mant = v;
                    v *= 10f64.powi(exp.parse::<i32>().ok()?);
                    // Overflow to inf, or underflow of a nonzero mantissa to 0, is no number.
                    if !v.is_finite() || (v == 0.0 && mant != 0.0) {
                        return None;
                    }
                    i = j;
                }
            }
            toks.push(Tok::Num(v));
        } else if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
            toks.push(if word == "of" { Tok::Of } else { Tok::Ident(word) });
        } else {
            toks.push(match c {
                '+' => Tok::Plus,
                '-' => Tok::Minus,
                '*' | '\u{d7}' => Tok::Star,
                '/' => Tok::Slash,
                '^' => Tok::Caret,
                '%' => Tok::Percent,
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                _ => return None,
            });
            i += 1;
        }
    }
    // `x` multiplies only between two numbers (so `x64dbg` stays a name).
    for i in 1..toks.len().saturating_sub(1) {
        if toks[i] == Tok::Ident("x".into())
            && matches!(toks[i - 1], Tok::Num(_))
            && matches!(toks[i + 1], Tok::Num(_))
        {
            toks[i] = Tok::Star;
        }
    }
    Some(toks)
}

/// A parsed value; `pct` marks a direct `n%` so `+`, `-` and `of` can treat it as a share.
#[derive(Clone, Copy)]
struct Val {
    v: f64,
    pct: bool,
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    depth: usize,
    meaningful: bool,
}

/// Rejects a non-finite intermediate result (`1/(10^400)` must not become 0).
fn chk(v: Val) -> Option<Val> {
    v.v.is_finite().then_some(v)
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn enter(&mut self) -> Option<()> {
        self.depth += 1;
        (self.depth <= MAX_DEPTH).then_some(())
    }

    fn expr(&mut self) -> Option<Val> {
        let mut lhs = self.term()?;
        while let Some(op @ (Tok::Plus | Tok::Minus)) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.term()?;
            self.meaningful = true;
            let r = if rhs.pct { lhs.v * rhs.v } else { rhs.v };
            lhs = chk(Val { v: if op == Tok::Plus { lhs.v + r } else { lhs.v - r }, pct: false })?;
        }
        Some(lhs)
    }

    fn term(&mut self) -> Option<Val> {
        let mut lhs = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::Star) | Some(Tok::Slash) => {
                    let mul = self.peek() == Some(&Tok::Star);
                    self.pos += 1;
                    let rhs = self.unary()?;
                    self.meaningful = true;
                    lhs = chk(Val { v: if mul { lhs.v * rhs.v } else { lhs.v / rhs.v }, pct: false })?;
                }
                Some(Tok::Of) => {
                    if !lhs.pct {
                        return None;
                    }
                    self.pos += 1;
                    let rhs = self.unary()?;
                    self.meaningful = true;
                    lhs = chk(Val { v: lhs.v * rhs.v, pct: false })?;
                }
                Some(Tok::Num(_)) | Some(Tok::Ident(_)) | Some(Tok::LParen) => {
                    // Implicit multiplication: a number or `)` directly followed by a name or `(`.
                    let left_ok = self.pos > 0
                        && matches!(self.toks[self.pos - 1], Tok::Num(_) | Tok::RParen);
                    let right_ok = matches!(self.peek(), Some(Tok::Ident(_)) | Some(Tok::LParen));
                    if lhs.pct || !left_ok || !right_ok {
                        return None;
                    }
                    let rhs = self.unary()?;
                    self.meaningful = true;
                    lhs = chk(Val { v: lhs.v * rhs.v, pct: false })?;
                }
                _ => return Some(lhs),
            }
        }
    }

    fn unary(&mut self) -> Option<Val> {
        match self.peek() {
            Some(Tok::Minus) | Some(Tok::Plus) => {
                let neg = self.peek() == Some(&Tok::Minus);
                self.pos += 1;
                self.enter()?;
                let v = self.unary();
                self.depth -= 1;
                let v = v?;
                Some(Val { v: if neg { -v.v } else { v.v }, pct: false })
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Option<Val> {
        let base = self.postfix()?;
        if self.peek() == Some(&Tok::Caret) {
            self.pos += 1;
            self.enter()?;
            let exp = self.unary();
            self.depth -= 1;
            self.meaningful = true;
            return chk(Val { v: base.v.powf(exp?.v), pct: false });
        }
        Some(base)
    }

    fn postfix(&mut self) -> Option<Val> {
        let a = self.atom()?;
        if self.peek() == Some(&Tok::Percent) {
            self.pos += 1;
            self.meaningful = true;
            return Some(Val { v: a.v / 100.0, pct: true });
        }
        Some(a)
    }

    /// `( expr )`, returning the inner value.
    fn paren(&mut self) -> Option<f64> {
        if self.peek() != Some(&Tok::LParen) {
            return None;
        }
        self.pos += 1;
        self.enter()?;
        let v = self.expr();
        self.depth -= 1;
        let v = v?;
        if self.peek() != Some(&Tok::RParen) {
            return None;
        }
        self.pos += 1;
        Some(v.v)
    }

    fn atom(&mut self) -> Option<Val> {
        let plain = |v| Some(Val { v, pct: false });
        match self.peek()?.clone() {
            Tok::Num(n) => {
                self.pos += 1;
                plain(n)
            }
            Tok::LParen => plain(self.paren()?),
            Tok::Ident(name) => {
                self.pos += 1;
                match name.as_str() {
                    "pi" => plain(std::f64::consts::PI),
                    "e" => plain(std::f64::consts::E),
                    _ => {
                        let f: fn(f64) -> f64 = match name.as_str() {
                            "sqrt" => f64::sqrt,
                            "abs" => f64::abs,
                            "round" => f64::round,
                            "floor" => f64::floor,
                            "ceil" => f64::ceil,
                            "ln" => f64::ln,
                            "log" => f64::log10,
                            "sin" => |x| x.to_radians().sin(),
                            "cos" => |x| x.to_radians().cos(),
                            "tan" => |x| x.to_radians().tan(),
                            _ => return None,
                        };
                        self.meaningful = true;
                        chk(Val { v: f(self.paren()?), pct: false })
                    }
                }
            }
            _ => None,
        }
    }
}

fn shown(toks: &[Tok], number: &dyn Fn(f64) -> String) -> String {
    let mut out = String::new();
    for (i, t) in toks.iter().enumerate() {
        let binary =
            i > 0 && matches!(toks[i - 1], Tok::Num(_) | Tok::RParen | Tok::Percent | Tok::Ident(_));
        let op = match t {
            Tok::Num(n) => {
                out.push_str(&number(*n));
                continue;
            }
            Tok::Ident(s) => {
                out.push_str(s);
                continue;
            }
            Tok::Percent => {
                out.push('%');
                continue;
            }
            Tok::LParen => {
                out.push('(');
                continue;
            }
            Tok::RParen => {
                out.push(')');
                continue;
            }
            Tok::Of => {
                out.push_str(" of ");
                continue;
            }
            Tok::Plus => '+',
            Tok::Minus => '-',
            Tok::Star => '*',
            Tok::Slash => '/',
            Tok::Caret => '^',
        };
        if binary {
            out.push_str(&format!(" {op} "));
        } else {
            out.push(op);
        }
    }
    out
}

/// Evaluates `text` when the whole of it is a calculation (not a bare number or word).
/// `shown` writes numbers with Rust's `Display`; see [`evaluate_with`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn evaluate(text: &str) -> Option<Calc> {
    evaluate_with(text, &|n| n.to_string())
}

/// Like [`evaluate`], writing each number of `shown` with `number` (the locale format).
pub fn evaluate_with(text: &str, number: &dyn Fn(f64) -> String) -> Option<Calc> {
    let toks = tokenize(text)?;
    if toks.is_empty() {
        return None;
    }
    let mut p = Parser { toks: &toks, pos: 0, depth: 0, meaningful: false };
    let v = p.expr()?;
    if p.pos != toks.len() || !p.meaningful || !v.v.is_finite() {
        return None;
    }
    Some(Calc { value: v.v, shown: shown(&toks, number) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates() {
        let v = |s| evaluate(s).map(|c| c.value);
        assert_eq!(v("12*7"), Some(84.0));
        assert_eq!(v("(3+4)^2"), Some(49.0));
        assert_eq!(v("2^3^2"), Some(512.0));
        assert_eq!(v("-3+5"), Some(2.0));
        assert_eq!(v("15% of 80"), Some(12.0));
        assert_eq!(v("80 + 15%"), Some(92.0));
        assert_eq!(v("5,5*2"), Some(11.0));
        assert_eq!(v("3x4"), Some(12.0));
        assert_eq!(v("sqrt(16)"), Some(4.0));
        assert!((v("2pi").unwrap() - std::f64::consts::TAU).abs() < 1e-12);
        assert!((v("sin(30)").unwrap() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn evaluates_more() {
        let v = |s| evaluate(s).map(|c| c.value);
        assert_eq!(v("80 - 15%"), Some(68.0));
        assert_eq!(v("50%"), Some(0.5));
        assert_eq!(v("2(3+1)"), Some(8.0));
        assert_eq!(v("-2^2"), Some(-4.0));
        assert_eq!(v("2*-3"), Some(-6.0));
        assert_eq!(v("3 × 4"), Some(12.0));
        assert_eq!(v("10 / 4"), Some(2.5));
        assert_eq!(v("SQRT(9) + abs(-1)"), Some(4.0));
        assert_eq!(v("floor(2.7)+ceil(2.1)+round(2.5)"), Some(8.0));
        assert_eq!(v("log(1000)"), Some(3.0));
        assert!((v("pi*2").unwrap() - std::f64::consts::TAU).abs() < 1e-12);
    }

    #[test]
    fn constants_and_scientific_notation() {
        let v = |s| evaluate(s).map(|c| c.value);
        for s in ["e2", "pi 2", "pi2", "2e3", "1/(10^400)", "1/2e400", "2e-400*1"] {
            assert!(evaluate(s).is_none(), "{s}");
        }
        assert_eq!(v("2e3*2"), Some(4000.0));
        assert_eq!(v("1,5e3 + 0"), Some(1500.0));
        assert_eq!(v("2e-3*1000"), Some(2.0));
        assert!((v("2pi").unwrap() - std::f64::consts::TAU).abs() < 1e-12);
        assert_eq!(v("2(3+1)"), Some(8.0));
        assert_eq!(v("(1+1)(2)"), Some(4.0));
    }

    #[test]
    fn shows_normalised_expression() {
        let s = |t: &str| evaluate(t).map(|c| c.shown);
        assert_eq!(s("12*7").as_deref(), Some("12 * 7"));
        assert_eq!(s("5,5*2").as_deref(), Some("5.5 * 2"));
        assert_eq!(s("3x4").as_deref(), Some("3 * 4"));
        assert_eq!(s("15%  of 80").as_deref(), Some("15% of 80"));
        assert_eq!(s("-3+5").as_deref(), Some("-3 + 5"));
        assert_eq!(s("sqrt(16)").as_deref(), Some("sqrt(16)"));
    }

    #[test]
    fn no_answer_for_invalid_maths() {
        for s in ["10^400", "1/0", "sqrt(-1)", "((", "2+", "*3", "ln(0)", "2 3", "5.", "50% 3", "3 of 4"] {
            assert!(evaluate(s).is_none(), "{s}");
        }
        assert!(evaluate(&format!("{}1{}", "(".repeat(5000), ")".repeat(5000))).is_none());
        assert!(evaluate(&format!("{}1+1", "-".repeat(5000))).is_none());
    }

    #[test]
    fn bare_numbers_and_words_are_not_questions() {
        for s in ["2024", "pi", "e", "x", "1.5", "-3", "(5)", ""] {
            assert!(evaluate(s).is_none(), "{s}");
        }
    }

    #[test]
    fn app_names_are_not_questions() {
        for s in [
            "7-Zip", "3D Viewer", "Win32 Disk Imager", "Paint 3D", "2048", "x64dbg", "Office 365", "Notepad++",
        ] {
            assert!(evaluate(s).is_none(), "{s}");
        }
    }
}
