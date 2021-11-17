use super::*;

#[derive(Debug, Clone, Default)]
pub struct Token {
    kind: TKind,
    value: Spam,
    line_data: LineData,
}

impl Token {
    pub fn builtin(value: &'static str) -> Self {
        Token {
            kind: TKind::Ident,
            value: Spam::infinite(value),
            line_data: LineData::default(),
        }
    }

    pub fn new(kind: TKind, value: Spam, line_data: LineData) -> Self {
        Token {
            kind,
            value,
            line_data,
        }
    }

    pub fn eof() -> Self {
        Token {
            kind: TKind::Eof,
            value: Spam::empty(),
            line_data: LineData::default(),
        }
    }

    pub fn kind(&self) -> TKind {
        self.kind.clone()
    }

    pub fn value(&self) -> &Spam {
        &self.value
    }

    pub fn line_data(&self) -> &LineData {
        &self.line_data
    }

    pub fn to_group(&mut self, end: &Token, trim: bool) {
        self.value = self.value.join(&end.value, trim);
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:?}", self.kind, self.value)?;
        Ok(())
    }
}

impl PartialEq<Token> for Token {
    fn eq(&self, other: &Token) -> bool {
        self.kind == other.kind && self.value == other.value
    }
}

impl PartialEq<TKind> for Token {
    fn eq(&self, other: &TKind) -> bool {
        self.kind == *other
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TKind {
    Fun,
    Attr,
    Pass,
    Mut,
    Return,
    If,
    Elif,
    Else,
    Var,
    Svar,
    Let,
    Loop,
    Break,
    Continue,
    Struct,
    Embed,

    Label,
    Ident,
    Op,

    LPar,
    RPar,
    LCurly,
    RCurly,
    LBra,
    RBra,
    Colon,
    Comma,
    RArrow,
    Hash,
    Dot,

    Int(i64, u16),
    Uint(u64, u16),
    Float(f64, u16),
    Bool(bool),
    Char(char),
    InvalidChar,
    String(Rc<Vec<u8>>),

    Indent(usize),

    Group,

    UnknownCharacter(char),
    Eof,
    None,
}

impl std::fmt::Display for TKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match *self {
            TKind::Fun => "'fun'",
            TKind::Attr => "'attr'",
            TKind::Pass => "'pass'",
            TKind::Mut => "'mut'",
            TKind::Return => "'return'",
            TKind::If => "'if'",
            TKind::Elif => "'elif'",
            TKind::Else => "'else'",
            TKind::Var => "'var'",
            TKind::Svar => "'svar'",
            TKind::Let => "'let'",
            TKind::Loop => "'loop'",
            TKind::Break => "'break'",
            TKind::Continue => "'continue'",
            TKind::Struct => "'struct'",
            TKind::Embed => "'embed'",
            TKind::Label => "'label'",
            TKind::Ident => "identifier",
            TKind::Op => "operator",
            TKind::LPar => "'('",
            TKind::RPar => "')'",
            TKind::LCurly => "'{'",
            TKind::RCurly => "'}'",
            TKind::LBra => "'['",
            TKind::RBra => "']'",
            TKind::Colon => "':'",
            TKind::Comma => "','",
            TKind::RArrow => "'->'",
            TKind::Dot => "'.'",
            TKind::Hash => "'#'",
            TKind::Indent(_) => "indentation",
            TKind::Int(..) => "integer",
            TKind::Uint(..) => "unsigned integer",
            TKind::Float(..) => "float",
            TKind::Bool(_) => "boolean",
            TKind::Char(_) => "character",
            TKind::InvalidChar => "invalid character",
            TKind::String(_) => "string",
            TKind::Group => "group",
            TKind::UnknownCharacter(_) => "unknown character",
            TKind::Eof => "end of file",
            TKind::None => "nothing",
        })
    }
}

impl Default for TKind {
    fn default() -> Self {
        TKind::None
    }
}
