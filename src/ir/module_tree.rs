use std::{fmt::Display, ops::Deref, path::Path};

use crate::{
    ast::{AEKind, AKind, AstError, AstParser},
    lexer::{Lexer, Token, TokenView},
    util::{self, sdbm::SdbmHashState},
};

use super::attributes::Attributes;
use super::*;

type Result<T> = std::result::Result<T, ModuleTreeError>;

pub struct ModuleTreeBuilder<'a> {
    import_stack: Vec<ID>,
    base: String,
    buffer: String,
    program: &'a mut Program,
    module_id_counter: u64,
    attributes: Attributes,
}

impl<'a> ModuleTreeBuilder<'a> {
    pub fn new(program: &'a mut Program) -> Self {
        ModuleTreeBuilder {
            import_stack: Vec::new(),
            base: String::new(),
            buffer: String::new(),
            program,
            module_id_counter: 0,
            attributes: Attributes::default(),
        }
    }

    pub fn build(mut self, root: &str) -> Result<()> {
        self.base = root[..root.rfind('/').map(|i| i + 1).unwrap_or(0)].to_string();
        self.load_module(
            &root[root.rfind('/').map(|i| i + 1).unwrap_or(0)
                ..root.rfind('.').unwrap_or(root.len())],
            &Token::default(),
        )?;

        Ok(())
    }

    fn load_module(&mut self, path: &str, token: &Token) -> Result<Mod> {
        self.load_path(path);

        let id = MOD_SALT.add(&self.buffer);

        if let Some(idx) = self.import_stack.iter().position(|&m| m == id) {
            let absolute_path = Path::new(self.buffer.as_str())
                .canonicalize()
                .map_err(|err| ModuleTreeError::new(MTEKind::Io(self.buffer.clone(), err), token))?
                .to_str()
                .ok_or_else(|| ModuleTreeError::new(MTEKind::NonUTF8Path, token))?
                .to_string();

            let message = self.import_stack[idx..]
                .iter()
                .map(|m| {
                    self.program
                        .modules
                        .get_id(*m)
                        .unwrap()
                        .absolute_path
                        .as_str()
                })
                .chain(std::iter::once(absolute_path.as_str()))
                .fold(String::new(), |mut acc, path| {
                    acc.push_str(path);
                    acc.push_str("\n");
                    acc
                });
            return Err(ModuleTreeError::new(
                MTEKind::CyclicDependency(message),
                token,
            ));
        }

        self.import_stack.push(id);

        if let Some(module) = self.program.modules.id_to_direct(id) {
            return Ok(module);
        }

        let file = std::fs::read_to_string(self.buffer.as_str())
            .map_err(|err| ModuleTreeError::new(MTEKind::Io(self.buffer.clone(), err), token))?;
        let absolute_path = Path::new(self.buffer.as_str())
            .canonicalize()
            .map_err(|err| ModuleTreeError::new(MTEKind::Io(self.buffer.clone(), err), token))?
            .to_str()
            .ok_or_else(|| ModuleTreeError::new(MTEKind::NonUTF8Path, token))?
            .to_string();

        let ast = AstParser::new(Lexer::new(path.to_string(), file))
            .parse()
            .map_err(Into::into)?;

        let name = Path::new(path)
            .file_stem()
            .ok_or_else(|| ModuleTreeError::new(MTEKind::NoFileStem, token))?
            .to_str()
            .ok_or_else(|| ModuleTreeError::new(MTEKind::NonUTF8Path, token))?;

        let name = MOD_SALT.add(name);

        let module = ModuleEnt {
            name,
            id,
            absolute_path,
            ast,

            dependency: vec![(MOD_SALT.add("builtin"), self.program.builtin)],

            ..Default::default()
        };

        let (_, module_id) = self.program.modules.insert(id, module);

        let mut ast = std::mem::take(&mut self.program[module_id].ast);
        util::try_retain(&mut ast, |a| {
            if let AKind::UseStatement(external) = a.kind {
                if external {
                    todo!("external package use not implemented");
                }
                let path = a[1].token.spam.deref();
                let m_id = self.load_module(&path[1..path.len() - 1], &a[1].token)?; // strip "
                let m = &mut self.program[m_id];
                let nickname = if a[0].kind != AKind::None {
                    MOD_SALT.add(a[0].token.spam.deref())
                } else {
                    m.name
                };
                m.dependant.push(module_id);
                let module = &mut self.program[module_id];
                module.dependency.push((nickname, m_id));
                Ok(false)
            } else {
                Ok(true)
            }
        })?;

        let attributes = self.attributes.resolve(&mut ast);

        let module = &mut self.program[module_id];
        module.ast = ast;
        module.attributes = attributes;

        self.import_stack
            .pop()
            .expect("expected previously pushed element");

        Ok(module_id)
    }

    fn load_path(&mut self, path: &str) {
        self.buffer.clear();
        self.buffer.push_str(self.base.as_str());
        self.buffer.push_str(path);
        self.buffer.push_str(crate::FILE_EXTENSION);
    }
}

#[derive(Debug)]
pub struct ModuleTreeError {
    pub kind: MTEKind,
    pub token: Token,
}

impl ModuleTreeError {
    pub fn new(kind: MTEKind, token: &Token) -> Self {
        Self {
            kind,
            token: token.clone(),
        }
    }
}

impl Display for ModuleTreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !matches!(self.kind, MTEKind::Ast(_)) {
            writeln!(f, "{}", TokenView::new(&self.token))?;
        }
        match &self.kind {
            MTEKind::Io(name, err) => writeln!(f, "cannot open file {:?}, cause: {}", name, err),
            MTEKind::NonUTF8Path => writeln!(f, "path contains non-utf8 characters"),
            MTEKind::NoFileStem => writeln!(f, "path has no file stem"),
            MTEKind::Ast(ast) => writeln!(f, "{}", AstError::new(ast.clone(), self.token.clone())),
            MTEKind::CyclicDependency(cycle) => {
                writeln!(f, "cyclic dependency detected:")?;
                writeln!(f, "{}", cycle)
            }
        }
    }
}

impl Into<ModuleTreeError> for AstError {
    fn into(self) -> ModuleTreeError {
        ModuleTreeError {
            kind: MTEKind::Ast(self.kind),
            token: self.token,
        }
    }
}

#[derive(Debug)]
pub enum MTEKind {
    Io(String, std::io::Error),
    Ast(AEKind),
    NonUTF8Path,
    NoFileStem,
    CyclicDependency(String),
}

pub fn test() {
    let mut program = Program::default();
    let builder = ModuleTreeBuilder::new(&mut program);
    builder.build("src/ir/tests/module_tree/root").unwrap();
}
