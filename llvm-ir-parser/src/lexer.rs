//! Hand-rolled LLVM IR lexer.

use std::fmt;

// ---------------------------------------------------------------------------
// Keyword enum
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Keyword {
    // Directives
    Source,       // source_filename
    Target,       // target
    Triple,       // triple
    Datalayout,   // datalayout
    Define,
    Declare,
    Type,         // "type" keyword after %Foo =

    // Linkage
    Private,
    Internal,
    External,
    Weak,
    WeakOdr,
    Linkonce,
    LinkonceOdr,
    Common,
    AvailableExternally,

    // Type keywords
    Void,
    Half,
    Bfloat,
    Float,
    Double,
    Fp128,
    X86Fp80,
    Label,
    Metadata,
    Ptr,

    // Storage
    Global,
    Constant,

    // Modifiers
    Inbounds,
    Exact,
    Nuw,
    Nsw,
    Volatile,
    Tail,
    Musttail,
    Notail,
    Fast,
    Nnan,
    Ninf,
    Nsz,
    Arcp,
    Contract,
    Afn,
    Reassoc,

    // Opcodes
    Add,  Sub,  Mul,  Udiv, Sdiv, Urem, Srem,
    And,  Or,   Xor,  Shl,  Lshr, Ashr,
    Fadd, Fsub, Fmul, Fdiv, Frem, Fneg,
    Icmp, Fcmp,
    Alloca, Load, Store, Getelementptr,
    Trunc, Zext, Sext, Fptrunc, Fpext,
    Fptoui, Fptosi, Uitofp, Sitofp,
    Ptrtoint, Inttoptr, Bitcast, Addrspacecast,
    Select, Phi, Extractvalue, Insertvalue,
    Extractelement, Insertelement, Shufflevector,
    Call,
    Ret, Br, Switch, Unreachable,

    // ICmp predicates
    Eq, Ne, Ugt, Uge, Ult, Ule, Sgt, Sge, Slt, Sle,
    // FCmp predicates
    False, Oeq, Ogt, Oge, Olt, Ole, One, Ord,
    Uno,   Ueq, Une, True,

    // Aggregate/misc constants
    Zeroinitializer,
    Undef,
    Poison,
    Null,
    Align,
    To,
    X,             // "x" in vector / array size
    Vscale,        // "vscale" before "x" in scalable vector
}

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    /// %name  (local identifier)
    LocalIdent(String),
    /// @name  (global identifier)
    GlobalIdent(String),
    /// iNN   (integer type, dynamic width)
    IntType(u32),
    /// Integer literal (signed)
    IntLit(i64),
    /// Unsigned integer literal (when value doesn't fit i64)
    UIntLit(u64),
    /// Float literal
    FloatLit(f64),
    /// String literal (inside double quotes)
    StringLit(String),
    /// Keyword
    Kw(Keyword),
    /// `=`
    Equal,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `*`
    Star,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `<`
    LAngle,
    /// `>`
    RAngle,
    /// `...`
    Ellipsis,
    /// `!`
    Bang,
    /// `#`
    Hash,
    /// End of input
    Eof,
}

// ---------------------------------------------------------------------------
// Lex error
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct LexError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lex error at {}:{}: {}", self.line, self.col, self.message)
    }
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

pub struct Lexer<'src> {
    src: &'src [u8],
    pos: usize,
    line: usize,
    col: usize,
    /// One-token lookahead.
    peeked: Option<Result<Token, LexError>>,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Lexer { src: src.as_bytes(), pos: 0, line: 1, col: 1, peeked: None }
    }

    // -----------------------------------------------------------------------
    // Character-level helpers
    // -----------------------------------------------------------------------

    fn peek_ch(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_ch2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let ch = self.src.get(self.pos).copied()?;
        self.pos += 1;
        if ch == b'\n' { self.line += 1; self.col = 1; } else { self.col += 1; }
        Some(ch)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            while matches!(self.peek_ch(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                self.advance();
            }
            if self.peek_ch() == Some(b';') {
                while !matches!(self.peek_ch(), None | Some(b'\n')) {
                    self.advance();
                }
            } else {
                break;
            }
        }
    }

    fn make_err(&self, msg: impl Into<String>) -> LexError {
        LexError { line: self.line, col: self.col, message: msg.into() }
    }

    // -----------------------------------------------------------------------
    // Public interface
    // -----------------------------------------------------------------------

    /// Peek at the next token without consuming it.
    pub fn peek(&mut self) -> Result<&Token, &LexError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.next_token());
        }
        match self.peeked.as_ref().unwrap() {
            Ok(t) => Ok(t),
            Err(e) => Err(e),
        }
    }

    /// Consume and return the next token.
    pub fn next(&mut self) -> Result<Token, LexError> {
        if let Some(t) = self.peeked.take() {
            return t;
        }
        self.next_token()
    }

    /// Consume if next token matches; otherwise leave it in the peek buffer.
    pub fn eat(&mut self, expected: &Token) -> bool {
        match self.peek() {
            Ok(t) if t == expected => { let _ = self.next(); true }
            _ => false,
        }
    }

    pub fn eat_kw(&mut self, kw: Keyword) -> bool {
        self.eat(&Token::Kw(kw))
    }

    /// Consume a token and verify it matches expected; return an error otherwise.
    pub fn expect(&mut self, expected: &Token) -> Result<(), LexError> {
        let tok = self.next()?;
        if tok == *expected {
            Ok(())
        } else {
            Err(self.make_err(format!("expected {:?}, got {:?}", expected, tok)))
        }
    }

    pub fn expect_kw(&mut self, kw: &Keyword) -> Result<(), LexError> {
        let tok = self.next()?;
        if tok == Token::Kw(kw.clone()) {
            Ok(())
        } else {
            Err(self.make_err(format!("expected keyword {:?}, got {:?}", kw, tok)))
        }
    }

    pub fn expect_local_ident(&mut self) -> Result<String, LexError> {
        match self.next()? {
            Token::LocalIdent(s) => Ok(s),
            t => Err(self.make_err(format!("expected local ident, got {:?}", t))),
        }
    }

    pub fn expect_global_ident(&mut self) -> Result<String, LexError> {
        match self.next()? {
            Token::GlobalIdent(s) => Ok(s),
            t => Err(self.make_err(format!("expected global ident, got {:?}", t))),
        }
    }

    pub fn expect_int_lit(&mut self) -> Result<i64, LexError> {
        match self.next()? {
            Token::IntLit(n) => Ok(n),
            Token::UIntLit(n) => Ok(n as i64),
            t => Err(self.make_err(format!("expected integer literal, got {:?}", t))),
        }
    }

    pub fn expect_uint_lit(&mut self) -> Result<u64, LexError> {
        match self.next()? {
            Token::IntLit(n) => Ok(n as u64),
            Token::UIntLit(n) => Ok(n),
            t => Err(self.make_err(format!("expected integer literal, got {:?}", t))),
        }
    }

    pub fn expect_string_lit(&mut self) -> Result<String, LexError> {
        match self.next()? {
            Token::StringLit(s) => Ok(s),
            t => Err(self.make_err(format!("expected string literal, got {:?}", t))),
        }
    }

    pub fn current_line(&self) -> usize { self.line }
    pub fn current_col(&self) -> usize { self.col }

    // -----------------------------------------------------------------------
    // Core tokenizer
    // -----------------------------------------------------------------------

    fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace_and_comments();

        let start_line = self.line;
        let start_col = self.col;

        let ch = match self.peek_ch() {
            None => return Ok(Token::Eof),
            Some(c) => c,
        };

        match ch {
            b'%' => {
                self.advance();
                let name = self.read_ident_or_int();
                Ok(Token::LocalIdent(name))
            }
            b'@' => {
                self.advance();
                let name = self.read_ident_or_int();
                Ok(Token::GlobalIdent(name))
            }
            b'"' => {
                self.advance();
                let s = self.read_string_literal()?;
                Ok(Token::StringLit(s))
            }
            b'-' | b'0'..=b'9' => self.lex_number(),
            b'=' => { self.advance(); Ok(Token::Equal) }
            b',' => { self.advance(); Ok(Token::Comma) }
            b':' => { self.advance(); Ok(Token::Colon) }
            b'*' => { self.advance(); Ok(Token::Star) }
            b'(' => { self.advance(); Ok(Token::LParen) }
            b')' => { self.advance(); Ok(Token::RParen) }
            b'[' => { self.advance(); Ok(Token::LBracket) }
            b']' => { self.advance(); Ok(Token::RBracket) }
            b'{' => { self.advance(); Ok(Token::LBrace) }
            b'}' => { self.advance(); Ok(Token::RBrace) }
            b'<' => { self.advance(); Ok(Token::LAngle) }
            b'>' => { self.advance(); Ok(Token::RAngle) }
            b'!' => { self.advance(); Ok(Token::Bang) }
            b'#' => { self.advance(); Ok(Token::Hash) }
            b'.' => {
                if self.src.get(self.pos+1) == Some(&b'.') && self.src.get(self.pos+2) == Some(&b'.') {
                    self.advance(); self.advance(); self.advance();
                    Ok(Token::Ellipsis)
                } else {
                    Err(LexError { line: start_line, col: start_col, message: "unexpected '.'".into() })
                }
            }
            _ if ch.is_ascii_alphabetic() || ch == b'_' || ch == b'$' => {
                let word = self.read_word();
                Ok(self.classify_word(&word))
            }
            _ => {
                self.advance();
                Err(LexError { line: start_line, col: start_col,
                    message: format!("unexpected character {:?}", ch as char) })
            }
        }
    }

    // -----------------------------------------------------------------------
    // Identifier / keyword scanning
    // -----------------------------------------------------------------------

    fn read_word(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek_ch() {
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'$' {
                self.advance();
                s.push(c as char);
            } else {
                break;
            }
        }
        s
    }

    /// After `%` or `@`, read an identifier (possibly quoted or numeric).
    fn read_ident_or_int(&mut self) -> String {
        if self.peek_ch() == Some(b'"') {
            self.advance();
            self.read_string_literal().unwrap_or_default()
        } else if self.peek_ch().map_or(false, |c| c.is_ascii_digit()) {
            let mut s = String::new();
            while let Some(c) = self.peek_ch() {
                if c.is_ascii_digit() { self.advance(); s.push(c as char); } else { break; }
            }
            s
        } else {
            let mut s = String::new();
            while let Some(c) = self.peek_ch() {
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'$' || c == b'-' {
                    self.advance();
                    s.push(c as char);
                } else {
                    break;
                }
            }
            s
        }
    }

    fn read_string_literal(&mut self) -> Result<String, LexError> {
        let mut s = String::new();
        loop {
            match self.peek_ch() {
                None => return Err(self.make_err("unterminated string")),
                Some(b'"') => { self.advance(); break; }
                Some(b'\\') => {
                    self.advance();
                    let h1 = self.advance().ok_or_else(|| self.make_err("bad escape"))?;
                    let h2 = self.advance().ok_or_else(|| self.make_err("bad escape"))?;
                    let hex_str = format!("{}{}", h1 as char, h2 as char);
                    let byte = u8::from_str_radix(&hex_str, 16).unwrap_or(0);
                    s.push(byte as char);
                }
                Some(c) => { self.advance(); s.push(c as char); }
            }
        }
        Ok(s)
    }

    // -----------------------------------------------------------------------
    // Number lexing
    // -----------------------------------------------------------------------

    fn lex_number(&mut self) -> Result<Token, LexError> {
        let negative = self.peek_ch() == Some(b'-');
        if negative { self.advance(); }

        // Hex float "0x..."
        if self.peek_ch() == Some(b'0') && matches!(self.peek_ch2(), Some(b'x') | Some(b'X')) {
            self.advance(); // '0'
            self.advance(); // 'x'
            let mut hex = String::new();
            while let Some(c) = self.peek_ch() {
                if c.is_ascii_hexdigit() { self.advance(); hex.push(c as char); } else { break; }
            }
            let bits = u64::from_str_radix(&hex, 16)
                .map_err(|_| self.make_err("bad hex float"))?;
            let f = f64::from_bits(bits);
            return Ok(Token::FloatLit(if negative { -f } else { f }));
        }

        // Read digits.
        let mut digits = String::new();
        while let Some(c) = self.peek_ch() {
            if c.is_ascii_digit() { self.advance(); digits.push(c as char); } else { break; }
        }

        // Float if decimal point or exponent follows.
        let is_float = matches!(self.peek_ch(), Some(b'.') | Some(b'e') | Some(b'E'));
        if is_float {
            let mut s = if negative { format!("-{}", digits) } else { digits };
            if self.peek_ch() == Some(b'.') {
                self.advance();
                s.push('.');
                while let Some(c) = self.peek_ch() {
                    if c.is_ascii_digit() { self.advance(); s.push(c as char); } else { break; }
                }
            }
            if matches!(self.peek_ch(), Some(b'e') | Some(b'E')) {
                self.advance();
                s.push('e');
                if matches!(self.peek_ch(), Some(b'+') | Some(b'-')) {
                    let sign = self.advance().unwrap();
                    s.push(sign as char);
                }
                while let Some(c) = self.peek_ch() {
                    if c.is_ascii_digit() { self.advance(); s.push(c as char); } else { break; }
                }
            }
            let f: f64 = s.parse().map_err(|_| self.make_err(format!("bad float: {}", s)))?;
            Ok(Token::FloatLit(f))
        } else {
            // Integer.
            let n: u64 = digits.parse().map_err(|_| self.make_err(format!("bad int: {}", digits)))?;
            if negative {
                Ok(Token::IntLit(-(n as i64)))
            } else if n <= i64::MAX as u64 {
                Ok(Token::IntLit(n as i64))
            } else {
                Ok(Token::UIntLit(n))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Keyword classification
    // -----------------------------------------------------------------------

    fn classify_word(&self, word: &str) -> Token {
        // iNN integer type?
        if let Some(bits_str) = word.strip_prefix('i') {
            if !bits_str.is_empty() && bits_str.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(bits) = bits_str.parse::<u32>() {
                    return Token::IntType(bits);
                }
            }
        }

        let kw = match word {
            "source_filename"      => Keyword::Source,
            "target"               => Keyword::Target,
            "triple"               => Keyword::Triple,
            "datalayout"           => Keyword::Datalayout,
            "define"               => Keyword::Define,
            "declare"              => Keyword::Declare,
            "type"                 => Keyword::Type,
            "private"              => Keyword::Private,
            "internal"             => Keyword::Internal,
            "external"             => Keyword::External,
            "weak"                 => Keyword::Weak,
            "weak_odr"             => Keyword::WeakOdr,
            "linkonce"             => Keyword::Linkonce,
            "linkonce_odr"         => Keyword::LinkonceOdr,
            "common"               => Keyword::Common,
            "available_externally" => Keyword::AvailableExternally,
            "void"                 => Keyword::Void,
            "half"                 => Keyword::Half,
            "bfloat"               => Keyword::Bfloat,
            "float"                => Keyword::Float,
            "double"               => Keyword::Double,
            "fp128"                => Keyword::Fp128,
            "x86_fp80"             => Keyword::X86Fp80,
            "label"                => Keyword::Label,
            "metadata"             => Keyword::Metadata,
            "ptr"                  => Keyword::Ptr,
            "global"               => Keyword::Global,
            "constant"             => Keyword::Constant,
            "inbounds"             => Keyword::Inbounds,
            "exact"                => Keyword::Exact,
            "nuw"                  => Keyword::Nuw,
            "nsw"                  => Keyword::Nsw,
            "volatile"             => Keyword::Volatile,
            "tail"                 => Keyword::Tail,
            "musttail"             => Keyword::Musttail,
            "notail"               => Keyword::Notail,
            "fast"                 => Keyword::Fast,
            "nnan"                 => Keyword::Nnan,
            "ninf"                 => Keyword::Ninf,
            "nsz"                  => Keyword::Nsz,
            "arcp"                 => Keyword::Arcp,
            "contract"             => Keyword::Contract,
            "afn"                  => Keyword::Afn,
            "reassoc"              => Keyword::Reassoc,
            "add"                  => Keyword::Add,
            "sub"                  => Keyword::Sub,
            "mul"                  => Keyword::Mul,
            "udiv"                 => Keyword::Udiv,
            "sdiv"                 => Keyword::Sdiv,
            "urem"                 => Keyword::Urem,
            "srem"                 => Keyword::Srem,
            "and"                  => Keyword::And,
            "or"                   => Keyword::Or,
            "xor"                  => Keyword::Xor,
            "shl"                  => Keyword::Shl,
            "lshr"                 => Keyword::Lshr,
            "ashr"                 => Keyword::Ashr,
            "fadd"                 => Keyword::Fadd,
            "fsub"                 => Keyword::Fsub,
            "fmul"                 => Keyword::Fmul,
            "fdiv"                 => Keyword::Fdiv,
            "frem"                 => Keyword::Frem,
            "fneg"                 => Keyword::Fneg,
            "icmp"                 => Keyword::Icmp,
            "fcmp"                 => Keyword::Fcmp,
            "alloca"               => Keyword::Alloca,
            "load"                 => Keyword::Load,
            "store"                => Keyword::Store,
            "getelementptr"        => Keyword::Getelementptr,
            "trunc"                => Keyword::Trunc,
            "zext"                 => Keyword::Zext,
            "sext"                 => Keyword::Sext,
            "fptrunc"              => Keyword::Fptrunc,
            "fpext"                => Keyword::Fpext,
            "fptoui"               => Keyword::Fptoui,
            "fptosi"               => Keyword::Fptosi,
            "uitofp"               => Keyword::Uitofp,
            "sitofp"               => Keyword::Sitofp,
            "ptrtoint"             => Keyword::Ptrtoint,
            "inttoptr"             => Keyword::Inttoptr,
            "bitcast"              => Keyword::Bitcast,
            "addrspacecast"        => Keyword::Addrspacecast,
            "select"               => Keyword::Select,
            "phi"                  => Keyword::Phi,
            "extractvalue"         => Keyword::Extractvalue,
            "insertvalue"          => Keyword::Insertvalue,
            "extractelement"       => Keyword::Extractelement,
            "insertelement"        => Keyword::Insertelement,
            "shufflevector"        => Keyword::Shufflevector,
            "call"                 => Keyword::Call,
            "ret"                  => Keyword::Ret,
            "br"                   => Keyword::Br,
            "switch"               => Keyword::Switch,
            "unreachable"          => Keyword::Unreachable,
            "eq"                   => Keyword::Eq,
            "ne"                   => Keyword::Ne,
            "ugt"                  => Keyword::Ugt,
            "uge"                  => Keyword::Uge,
            "ult"                  => Keyword::Ult,
            "ule"                  => Keyword::Ule,
            "sgt"                  => Keyword::Sgt,
            "sge"                  => Keyword::Sge,
            "slt"                  => Keyword::Slt,
            "sle"                  => Keyword::Sle,
            "false"                => Keyword::False,
            "oeq"                  => Keyword::Oeq,
            "ogt"                  => Keyword::Ogt,
            "oge"                  => Keyword::Oge,
            "olt"                  => Keyword::Olt,
            "ole"                  => Keyword::Ole,
            "one"                  => Keyword::One,
            "ord"                  => Keyword::Ord,
            "uno"                  => Keyword::Uno,
            "ueq"                  => Keyword::Ueq,
            "une"                  => Keyword::Une,
            "true"                 => Keyword::True,
            "zeroinitializer"      => Keyword::Zeroinitializer,
            "undef"                => Keyword::Undef,
            "poison"               => Keyword::Poison,
            "null"                 => Keyword::Null,
            "align"                => Keyword::Align,
            "to"                   => Keyword::To,
            "x"                    => Keyword::X,
            "vscale"               => Keyword::Vscale,
            // Unknown words become bare local identifiers (shouldn't normally happen at module level).
            other => return Token::LocalIdent(other.to_string()),
        };
        Token::Kw(kw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_all(src: &str) -> Vec<Token> {
        let mut lex = Lexer::new(src);
        let mut toks = Vec::new();
        loop {
            let t = lex.next().unwrap();
            if t == Token::Eof { break; }
            toks.push(t);
        }
        toks
    }

    #[test]
    fn lex_basic_tokens() {
        let toks = lex_all("define i32 @foo(i32 %x) {");
        assert_eq!(toks[0], Token::Kw(Keyword::Define));
        assert_eq!(toks[1], Token::IntType(32));
        assert_eq!(toks[2], Token::GlobalIdent("foo".to_string()));
        assert_eq!(toks[3], Token::LParen);
        assert_eq!(toks[4], Token::IntType(32));
        assert_eq!(toks[5], Token::LocalIdent("x".to_string()));
        assert_eq!(toks[6], Token::RParen);
        assert_eq!(toks[7], Token::LBrace);
    }

    #[test]
    fn lex_integer_literals() {
        let toks = lex_all("42 -7 0");
        assert_eq!(toks[0], Token::IntLit(42));
        assert_eq!(toks[1], Token::IntLit(-7));
        assert_eq!(toks[2], Token::IntLit(0));
    }

    #[test]
    fn lex_hex_float() {
        let toks = lex_all("0x4000000000000000");
        assert_eq!(toks[0], Token::FloatLit(2.0));
    }

    #[test]
    fn lex_comment_skipped() {
        let toks = lex_all("; this is a comment\ni32");
        assert_eq!(toks[0], Token::IntType(32));
    }

    #[test]
    fn lex_string_literal() {
        let toks = lex_all("\"hello\"");
        assert_eq!(toks[0], Token::StringLit("hello".to_string()));
    }

    #[test]
    fn lex_ellipsis() {
        let toks = lex_all("...");
        assert_eq!(toks[0], Token::Ellipsis);
    }

    #[test]
    fn lex_keywords() {
        let toks = lex_all("ret void add nsw");
        assert_eq!(toks[0], Token::Kw(Keyword::Ret));
        assert_eq!(toks[1], Token::Kw(Keyword::Void));
        assert_eq!(toks[2], Token::Kw(Keyword::Add));
        assert_eq!(toks[3], Token::Kw(Keyword::Nsw));
    }
}
