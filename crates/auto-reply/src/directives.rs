/// Parse inline directives from message text (e.g. #think, #command, #reset).
pub struct Directive {
    pub kind: DirectiveKind,
    pub value: Option<String>,
}

pub enum DirectiveKind {
    Think,
    Command,
    Reset,
}

pub fn parse_directives(_text: &str) -> Vec<Directive> {
    todo!("scan message for #directive patterns")
}
