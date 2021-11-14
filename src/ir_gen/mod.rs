pub mod gen;

use cranelift_codegen::{binemit::{NullStackMapSink, NullTrapSink}, entity::EntityRef, ir::{AbiParam, Block, ExternalName, FuncRef, Function, InstBuilder, Signature, Type, Value, condcodes::{FloatCC, IntCC}, types::*}, isa::CallConv};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::ObjectModule;
use std::{ops::{Deref, DerefMut}, str::FromStr};

use crate::{
    ast::{AEKind, AKind, Ast, AstError, AstParser},
    lexer::{Lexer, StrRef, TKind, Token},
};

type CraneContext = cranelift_codegen::Context;
type Result<T> = std::result::Result<T, IrGenError>;
type ExprResult = Result<Option<(Value, Cell<Datatype>)>>;
type LoopHeader = (StrRef, Block, Block, Option<Option<Cell<Datatype>>>);

pub struct Generator {
    builtin_repo: BuiltinRepo,
    builtin_module: Cell<Mod>,
    context: Context,
    current_module: Cell<Mod>,
    variables: Vec<Option<Var>>,
    loop_headers: Vec<LoopHeader>,
    variable_counter: usize,
    global_attributes: Vec<Ast>,
    pushed_attributes: Vec<Ast>,
    imported_functions: Vec<(StrRef, FuncRef)>,
    call_buffer: Vec<Value>,
    object_module: ObjectModule,
}

impl Generator {
    fn new(object_module: ObjectModule) -> Self {
        let builtin_repo = BuiltinRepo::new();
        let builtin_module = builtin_repo.to_module();
        Self {
            current_module: builtin_module.clone(), // just an place holder
            builtin_repo,
            builtin_module,
            context: Context::new(),
            variables: Vec::new(),
            loop_headers: Vec::new(),
            variable_counter: 0,
            global_attributes: Vec::new(),
            pushed_attributes: Vec::new(),
            imported_functions: Vec::new(),
            call_buffer: Vec::new(),
            object_module,
        }
    }

    fn generate(mut self, root_file_name: &str) -> Result<ObjectModule> {
        self.generate_module(root_file_name.to_string())?;

        let mut codegen_ctx = cranelift_codegen::Context::new();
        let mut function_context = FunctionBuilderContext::new();
        let ctx = std::mem::replace(&mut self.context, Context::new());
        for mut f in ctx.modules.iter().map(|m| m.functions.iter()).flatten().map(Clone::clone) {
            if let AKind::None = f.body.kind {
                continue;
            }
            self.variables.clear();
            self.imported_functions.clear();
            self.variable_counter = 0;
            let mut function = Function::with_name_signature(
                ExternalName::default(), 
                std::mem::take(&mut f.signature).unwrap(),
            );
            let mut builder = FunctionBuilder::new(&mut function, &mut function_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);

            for (value, var) in builder.block_params(entry_block).iter().zip(f.params.iter_mut()) {
                var.access = VarAccess::Immutable(value.clone());
                self.variables.push(Some(var.clone()));
            }

            self.generate_function_body(&f.body, &mut builder)?;
            builder.seal_current_block();
            builder.finalize();
            codegen_ctx.func = function;
            codegen_ctx.compute_cfg();
            codegen_ctx.compute_domtree();
            self.object_module.define_function(
                f.id, 
                &mut codegen_ctx, 
                &mut NullTrapSink::default(), 
                &mut NullStackMapSink {},
            ).unwrap();
            
        };

        Ok(self.object_module)
    }

    fn generate_module(&mut self, module_path: String) -> Result<Cell<Mod>> {
        let mut ast = self.load_ast(module_path)?;

       
        self.current_module = Cell::new(Mod::new(
            ast.token.line_data.file_name.to_string(),
        ));
        self.current_module
            .dependency
            .push(self.builtin_module.clone());
        for mut item in ast.drain(..) {
            match item.kind {
                AKind::Function => {
                    self.function(item)?;
                    self.pushed_attributes.clear();
                }
                AKind::Attribute => match item[0].token.value.deref() {
                    "push" => {
                        self.global_attributes.push(Ast::none());
                        item.drain(..)
                            .for_each(|item| self.global_attributes.push(item));
                    }
                    "pop" => {
                        self.global_attributes.drain(
                            self.global_attributes
                                .iter()
                                .rev()
                                .position(|e| e.kind == AKind::None)
                                .unwrap()..,
                        );
                    }
                    _ => {
                        item.drain(..)
                            .for_each(|item| self.pushed_attributes.push(item));
                    }
                },
                _ => todo!(),
            }
        }

        self.context.add_module(self.current_module.clone())?;

        Ok(self.current_module.clone())
    }

    fn function(&mut self, ast: Ast) -> Result<()> {
        let fun = self.create_signature(ast)?;
        self.current_module.add_function(fun)?;
        Ok(())
    }

    fn generate_function_body(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> Result<()> {
        self.statement_list(ast, builder)?;
        Ok(())
    }

    fn statement_list(
        &mut self,
        ast: &Ast,
        builder: &mut FunctionBuilder,
    ) -> ExprResult {
        for stmt in ast[0..ast.len() - 1].iter() {
            self.statement(stmt, builder)?;
        }

        if let Some(last) = ast.last() {
            return self.statement(last, builder);
        }

        Ok(None)
    }

    fn statement(
        &mut self,
        ast: &Ast,
        builder: &mut FunctionBuilder,
    ) -> ExprResult {
        match ast.kind {
            AKind::ReturnStatement => self.return_statement(ast, builder)?,
            AKind::VarStatement(_) => self.var_statement(ast, builder)?,
            AKind::Break => self.break_statement(ast, builder)?,
            
            AKind::IfExpression => return self.if_expression(ast, builder),
            AKind::Call => return self.call_expression(ast, builder),
            AKind::Loop => return self.loop_expression(ast, builder),
            _ => {
                return Ok(Some(self.expression(ast, builder)?));
            }
        }

        Ok(None)
    }

    fn break_statement(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> Result<()> {
        let mut header = self.find_loop_header(&ast[0].token)?;
        let loop_exit_block = header.2;
        if let Some(datatype) = header.3 {
            if let Some(datatype) = datatype {
                if ast[1].kind == AKind::None {
                    return Err(IrGenError::new(IGEKind::ExpectedValue, ast.token.clone()))
                }
                let (value, real_datatype) = self.expression(&ast[1], builder)?;
                self.assert_type(ast, &real_datatype, &datatype)?;
                builder.ins().jump(loop_exit_block.clone(), &[value]);
            } else {
                if ast[1].kind != AKind::None {
                    return Err(IrGenError::new(IGEKind::UnexpectedValue, ast.token.clone()))
                }
                builder.ins().jump(loop_exit_block.clone(), &[]); 
            }
        } else {
            header.3 = if ast[1].kind == AKind::None {
                builder.ins().jump(loop_exit_block.clone(), &[]);
                Some(None)
            } else {
                let (value, real_datatype) = self.expression(&ast[1], builder)?;
                builder.ins().jump(loop_exit_block.clone(), &[value]);
                Some(Some(real_datatype))
            };
            self.update_loop_header(header);
        }
        Ok(())
    }

    fn find_loop_header(&mut self, name: &Token) -> Result<LoopHeader> {
        self.loop_headers
            .iter()
            .rev()
            .find(|(header_name, ..)| header_name.deref() == name.value.deref())
            .map(|h| h.clone())
            .ok_or_else(|| IrGenError::new(
                IGEKind::LoopHeaderNotFound,
                name.clone(),
            ))
    }

    fn update_loop_header(&mut self, header: LoopHeader) {
        self.loop_headers
            .iter_mut()
            .find(|(header_name, ..)| header_name.deref() == header.0.deref())
            .map(|h| *h = header);
    }

    fn return_statement(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> Result<()> {
        let ret_expr = &ast[0];

        let ret_value = self.expression(ret_expr, builder)?;

        builder.ins().return_(&[ret_value.0]);

        Ok(())
    }

    fn expression(
        &mut self,
        ast: &Ast,
        builder: &mut FunctionBuilder,
    ) -> Result<(Value, Cell<Datatype>)> {
        match ast.kind {
            AKind::Literal => return self.literal(ast, builder),
            AKind::Identifier => {
                let var = self.find_variable(&ast.token)?;
                let val = match var.access {
                    VarAccess::Mutable(var) => builder.use_var(var),
                    VarAccess::Immutable(val) => val,
                    _ => unreachable!(),
                };
                return Ok((val, var.datatype.clone()))
            }

            AKind::BinaryOperation => self.binary_operation(ast, builder)?,
            AKind::IfExpression => self.if_expression(ast, builder)?,
            AKind::Call => self.call_expression(&ast, builder)?,
            AKind::Loop => self.loop_expression(ast, builder)?,
            _ => todo!("unmatched expression {}", ast),
        }.ok_or_else(|| IrGenError::new(IGEKind::ExpectedValue, ast.token.clone()))
    }

    fn loop_expression(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> ExprResult {
        let loop_block = builder.create_block();
        let loop_exit_block = builder.create_block();

        self.loop_headers.push((ast[0].token.value.clone(), loop_block, loop_exit_block, None));

        builder.ins().jump(loop_block, &[]);
        builder.seal_current_block();
        builder.switch_to_block(loop_block);
        self.statement_list(&ast[1], builder)?;
        builder.ins().jump(loop_block, &[]);
        builder.seal_block(loop_block);
        builder.seal_current_block();

        builder.switch_to_block(loop_exit_block);

        Ok(if let Some((.., Some(Some(datatype)))) = self.loop_headers.pop() {
            let value = builder.append_block_param(loop_exit_block, datatype.get_ir_repr());
            Some((value, datatype))
        } else {
            None
        })
    }

    fn var_statement(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> Result<()> {
        let mutable = ast.kind == AKind::VarStatement(true);
        for var in ast.iter() {
            let (identifiers, datatype, values) = (&var[0], &var[1], &var[2]);
            
            let mut datatype = if let AKind::None = datatype.kind {
                None
            } else {
                Some(self.find_datatype(&datatype.token)?)
            };

            for (i, ident) in identifiers.iter().map(|i| i.token.value.clone()).enumerate() {
                let value = if values.kind != AKind::None {
                    let (value, real_datatype) = self.expression(&values[i], builder)?;
                    if let Some(datatype) = datatype.as_ref() {
                        self.assert_type(&values[i], &real_datatype, datatype)?;   
                    } else {
                        datatype = Some(real_datatype);
                    }
                    value
                } else {
                    datatype.as_ref().unwrap().default_value(builder)
                };

                let access = if mutable {
                    let var = Variable::new(self.variable_counter);
                    self.variable_counter += 1;
                    builder.declare_var(var, datatype.as_ref().unwrap().get_ir_repr());
                    builder.def_var(var, value);
                    VarAccess::Mutable(var)
                } else {
                    VarAccess::Immutable(value)
                };

                self.variables.push(Some(Var::new(
                    ident, 
                    access,  
                    datatype.clone().unwrap(),
                )));
            }
        }

        Ok(())
    }

    fn call_expression(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> ExprResult {
        let fun = self.find_function(&ast[0].token)?;

        let fun_ref = self.imported_functions
            .iter()
            .find(|(id, _)| id == &fun.name)
            .map(|(_, fun_ref)| *fun_ref)
            .unwrap_or_else(|| {
                let fun_ref = self.object_module.declare_func_in_func(fun.id, builder.func);
                self.imported_functions.push((fun.name.clone(), fun_ref));
                fun_ref
            });

        if fun.params.len() != ast.len() - 1 {
            return Err(IrGenError::new(IGEKind::InvalidAmountOfArguments(
                fun.params.len(), ast.len() - 1), ast.token.clone()))
        }

        for (i, e) in ast[1..].iter().enumerate() {
            let (value, datatype) = self.expression(e, builder)?;
            self.assert_type(e, &datatype, &fun.params[i].datatype)?;
            self.call_buffer.push(value);
        }

        let vals = builder.ins().call(fun_ref, &self.call_buffer);
        self.call_buffer.clear();

        if let Some(ret) = fun.ret.as_ref() {
            Ok(Some((builder.inst_results(vals)[0], ret.clone())))
        } else {
            Ok(None)
        }
    }

    fn literal(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> Result<(Value, Cell<Datatype>)> {
        match ast.token.kind {
            TKind::Int(value, bits) => {
                let datatype = match bits {
                    8 => self.builtin_repo.i8.clone(),
                    16 => self.builtin_repo.i16.clone(),
                    32 => self.builtin_repo.i32.clone(),
                    64 => self.builtin_repo.i64.clone(),
                    _ => unreachable!(),
                };
                let value = builder
                    .ins()
                    .iconst(datatype.get_ir_repr(), value as i64);
                Ok((value, datatype))
            }
            TKind::Bool(value) => {
                let datatype = self.builtin_repo.bool.clone();
                let value = builder.ins().bconst(datatype.get_ir_repr(), value);
                Ok((value, datatype))
            }
            TKind::Char(value) => {
                let datatype = self.builtin_repo.i32.clone();
                let value = builder.ins().iconst(datatype.get_ir_repr(), value as i64);
                Ok((value, datatype))
            }
            _ => todo!("unmatched literal token {:?}", ast.token),
        }
    }

    fn if_expression(
        &mut self,
        ast: &Ast,
        builder: &mut FunctionBuilder,
    ) -> ExprResult {
        let cond_expr = &ast[0];
        let (cond_value, cond_type) = self.expression(cond_expr, builder)?;

        self.assert_type(cond_expr, &cond_type, &self.builtin_repo.bool)?;

        let then_block = builder.create_block();
        builder.ins().brnz(cond_value, then_block, &[]);

        let mut merge_block = None;

        let else_branch = &ast[2];
        let else_block = if else_branch.kind == AKind::None {
            let block = builder.create_block();
            builder.ins().jump(block, &[]);
            merge_block = Some(block);
            None
        } else {
            let else_block = builder.create_block();
            builder.ins().jump(else_block, &[]);
            Some(else_block)
        };

        if let Some((_, loop_block, ..)) = self.loop_headers.last() {
            if loop_block.to_owned() != builder.current_block().unwrap() {
                builder.seal_current_block();
            }
        } else {
            builder.seal_current_block();
        }

        let then_branch = &ast[1];

        builder.switch_to_block(then_block);
        let then_result = self.statement_list(then_branch, builder)?;

        let mut result = None;
        let mut then_filled = false;
        if let Some((value, datatype)) = then_result {
            if merge_block.is_some() {
                return Err(IrGenError::new(
                    IGEKind::MissingElseBranch,
                    ast.token.clone(),
                ));
            }
            let block = builder.create_block();
            let val = builder.append_block_param(block, datatype.get_ir_repr());
            result = Some((val, datatype));
            builder.ins().jump(block, &[value]);
            merge_block = Some(block);
        } else if !builder.is_filled() {
            let block = merge_block.unwrap_or_else(|| builder.create_block());
            builder.ins().jump(block, &[]);
            merge_block = Some(block);
        } else {
            then_filled = true;
        }

        builder.seal_current_block();

        if else_branch.kind == AKind::Group {
            let else_block = else_block.unwrap();

            builder.switch_to_block(else_block);
            let else_result = self.statement_list(else_branch, builder)?;
                
            if let Some((value, datatype)) = else_result {
                if let Some((_, other_datatype)) = result.as_ref() {
                    self.assert_type(ast, &datatype, &other_datatype)?;
                    builder.ins().jump(merge_block.unwrap(), &[value]);
                } else if then_filled {
                    let block = builder.create_block();
                    let val = builder.append_block_param(block, datatype.get_ir_repr());
                    result = Some((val, datatype));
                    builder.ins().jump(block, &[value]);
                    merge_block = Some(block);
                } else {
                    return Err(IrGenError::new(
                        IGEKind::UnexpectedValueInThenBranch,
                        ast.token.clone(),
                    ));
                }
            } else {
                if result.is_some() {
                    return Err(IrGenError::new(
                        IGEKind::MissingValueInElseBranch,
                        ast.token.clone(),
                    ));
                }
                if !builder.is_filled() {
                    let block = merge_block.unwrap_or_else(|| builder.create_block());
                    builder.ins().jump(block, &[]);
                    merge_block = Some(block);
                }
            }

            if merge_block.is_some() {
                builder.seal_current_block();
            }
        }

        if let Some(block) = merge_block {
            builder.switch_to_block(block);
        }

        Ok(result)
    }

    fn binary_operation(
        &mut self,
        ast: &Ast,
        builder: &mut FunctionBuilder,
    ) -> ExprResult {
        let op = ast[0].token.value.deref();

        if op == "=" {
            return Ok(Some(self.assign(ast, builder)?));
        }

        let (left_val, left_type) = self.expression(&ast[1], builder)?;
        let (right_val, right_type) = self.expression(&ast[2], builder)?;

        if left_type == right_type {
            let value = match left_type.name.as_str() {
                "i8" | "i16" | "i32" | "i64" => match op {
                    "+" => builder.ins().iadd(left_val, right_val),
                    "-" => builder.ins().isub(left_val, right_val),
                    "*" => builder.ins().imul(left_val, right_val),
                    "/" => builder.ins().sdiv(left_val, right_val),
                    "%" => builder.ins().srem(left_val, right_val),
                    "&" => builder.ins().band(left_val, right_val),
                    "|" => builder.ins().bor(left_val, right_val),
                    "^" => builder.ins().bxor(left_val, right_val),
                    "<<" => builder.ins().ishl(left_val, right_val),
                    ">>" => builder.ins().sshr(left_val, right_val),
                    "max" => builder.ins().imax(left_val, right_val),
                    "min" => builder.ins().imin(left_val, right_val),

                    "==" | "!=" | "<" | ">" | ">=" | "<=" => {
                        let op = match op {
                            "==" => IntCC::Equal,
                            "!=" => IntCC::NotEqual,
                            "<" => IntCC::SignedLessThan,
                            ">" => IntCC::SignedGreaterThan,
                            ">=" => IntCC::SignedGreaterThanOrEqual,
                            "<=" => IntCC::SignedLessThanOrEqual,
                            _ => unreachable!(),
                        };

                        let val = builder.ins().icmp(op, left_val, right_val);
                        return Ok(Some((val, self.builtin_repo.bool.clone())));
                    }

                    _ => todo!("unsupported int operator {}", op),
                },
                "u8" | "u16" | "u32" | "u64" => match op {
                    "+" => builder.ins().iadd(left_val, right_val),
                    "-" => builder.ins().isub(left_val, right_val),
                    "*" => builder.ins().imul(left_val, right_val),
                    "/" => builder.ins().udiv(left_val, right_val),
                    "%" => builder.ins().urem(left_val, right_val),
                    "&" => builder.ins().band(left_val, right_val),
                    "|" => builder.ins().bor(left_val, right_val),
                    "^" => builder.ins().bxor(left_val, right_val),
                    "<<" => builder.ins().ishl(left_val, right_val),
                    ">>" => builder.ins().ushr(left_val, right_val),
                    "max" => builder.ins().umax(left_val, right_val),
                    "min" => builder.ins().umin(left_val, right_val),

                    "==" | "!=" | "<" | ">" | ">=" | "<=" => {
                        let op = match op {
                            "==" => IntCC::Equal,
                            "!=" => IntCC::NotEqual,
                            "<" => IntCC::UnsignedLessThan,
                            ">" => IntCC::UnsignedGreaterThan,
                            ">=" => IntCC::UnsignedGreaterThanOrEqual,
                            "<=" => IntCC::UnsignedLessThanOrEqual,
                            _ => unreachable!(),
                        };

                        let val = builder.ins().icmp(op, left_val, right_val);
                        return Ok(Some((val, self.builtin_repo.bool.clone())));
                    }

                    _ => todo!("unsupported uint operator {}", op),
                },
                "f32" | "f64" => match op {
                    "+" => builder.ins().fadd(left_val, right_val),
                    "-" => builder.ins().fsub(left_val, right_val),
                    "*" => builder.ins().fmul(left_val, right_val),
                    "/" => builder.ins().fdiv(left_val, right_val),
                    "max" => builder.ins().fmax(left_val, right_val),
                    "min" => builder.ins().fmin(left_val, right_val),

                    "==" | "=!" | "<" | ">" | ">=" | "<=" => {
                        let op = match op {
                            "==" => FloatCC::Equal,
                            "!=" => FloatCC::NotEqual,
                            "<" => FloatCC::LessThan,
                            ">" => FloatCC::GreaterThan,
                            ">=" => FloatCC::GreaterThanOrEqual,
                            "<=" => FloatCC::LessThanOrEqual,
                            _ => unreachable!(),
                        };

                        let val = builder.ins().fcmp(op, left_val, right_val);
                        return Ok(Some((val, self.builtin_repo.bool.clone())));
                    }
                    _ => todo!("unsupported float operation {}", op),
                },

                "bool" => match op {
                    "&" => builder.ins().band(left_val, right_val),
                    "|" => builder.ins().bor(left_val, right_val),
                    "||" => todo!("unsupported ||"),
                    "&&" => todo!("unsupported &&"),
                    _ => todo!("unsupported bool operation {}", op),
                },

                _ => todo!("unmatched operator {} on type {}", op, right_type.name.as_str()),
            };
            Ok(Some((value, right_type)))
        } else {
            todo!("non-matching type of binary operation")
        }
    }

    fn assign(&mut self, ast: &Ast, builder: &mut FunctionBuilder) -> Result<(Value, Cell<Datatype>)> {
        let var = self.find_variable(&ast[1].token)?.clone();

        let (val, datatype) = self.expression(&ast[2], builder)?;

        self.assert_type(&ast, &datatype, &var.datatype)?;

        if let VarAccess::Mutable(var) = var.access {
            builder.def_var(var, val);
        } else {
            return Err(IrGenError::new(IGEKind::AssignToImmutable, ast.token.clone()));
        }

        Ok((val, datatype))
    }

    fn create_signature(&mut self, mut ast: Ast) -> Result<Cell<Fun>> {
        let header = &ast[0];
        let mut signature = Signature::new(CallConv::Fast);
        let mut fun_params = Vec::new();
        for args in header[1..header.len() - 1].iter() {
            let datatype = self.find_datatype(&args.last().unwrap().token)?;
            for arg in args[0..args.len() - 1].iter() {
                fun_params.push(Var::new(
                    arg.token.value.clone(),
                    VarAccess::Unresolved,
                    datatype.clone(),
                ));
                signature
                    .params
                    .push(AbiParam::new(datatype.get_ir_repr()));
            }
        }
        let return_type = header.last().unwrap();
        let return_type = if return_type.kind != AKind::None {
            let datatype = self.find_datatype(&return_type.token)?.clone();
            signature
                .returns
                .push(AbiParam::new(datatype.get_ir_repr()));
            Some(datatype)
        } else {
            None
        };
        let name = header.first().unwrap();
        let name = if name.kind != AKind::None {
            name.token.value.clone()
        } else {
            StrRef::empty()
        };

        let linkage = if let Some(attr) = self.find_attribute("linkage") {
            self.assert_atr_len(attr, 1)?;
            match attr[0].token.value.deref() {
                "local" => Linkage::Local,
                "hidden" => Linkage::Hidden,
                "import" => Linkage::Import,
                "export" => Linkage::Export,
                "preemptible" => Linkage::Preemptible,
                _ => return Err(IrGenError::new(IGEKind::InvalidLinkage, attr.token.clone())),
            }
        } else {
            Linkage::Export
        };

        let call_conv = if let Some(attr) = self.find_attribute("call_conv") {
            self.assert_atr_len(attr, 1)?;
            CallConv::from_str(attr[0].token.value.deref())
                .map_err(|_| IrGenError::new(IGEKind::InvalidCallConv, attr.token.clone()))?
        } else {
            CallConv::Fast
        };

        signature.call_conv = call_conv;

        let inline_level = if let Some(attr) = self.find_attribute("inline") {
            self.assert_atr_len(attr, 1)?;
            InlineLevel::from_str(attr[0].token.value.deref())
                .map_err(|_| IrGenError::new(IGEKind::InvalidInlineLevel, attr.token.clone()))?
        } else {
            InlineLevel::Never
        };

        signature.call_conv = call_conv;

        let fun = Fun {
            id: self.object_module.declare_function(name.deref(), linkage, &signature).unwrap(),
            name,
            params: fun_params,
            ret: return_type,
            inline_level,
            signature: Some(signature),
            body: ast.remove(1),
        };

        Ok(Cell::new(fun))
    }

    fn assert_type(&self, ast: &Ast, actual: &Cell<Datatype>, expected: &Cell<Datatype>) -> Result<()> {
        if actual != expected {
            Err(IrGenError::new(
                IGEKind::TypeMismatch(actual.clone(), expected.clone()),
                ast.token.clone(),
            ))
        } else {
            Ok(())
        }
    }

    fn find_datatype(&self, token: &Token) -> Result<Cell<Datatype>> {
        self.current_module
            .find_datatype(&token.value)
            .ok_or_else(|| IrGenError::new(IGEKind::TypeNotFound, token.clone()))
    }

    fn find_function(&self, token: &Token) -> Result<Cell<Fun>> {
        self.current_module
            .find_function(token)
            .ok_or_else(|| IrGenError::new(IGEKind::FunctionNotFound, token.clone()))
    }

    fn find_variable(&self, token: &Token) -> Result<&Var> {
        self.variables
            .iter()
            .rev()
            .filter(|v| v.is_some())
            .map(|v| v.as_ref().unwrap())
            .find(|var| var.name.deref() == token.value.deref())
            .ok_or_else(|| IrGenError::new(IGEKind::VariableNotFound, token.clone()))
    }

    fn load_ast(&mut self, file_name: String) -> Result<Ast> {
        let bytes =
            std::fs::read_to_string(&file_name).map_err(|e| IGEKind::CannotOpenFile(e).into())?;
        AstParser::new(Lexer::new(file_name, bytes))
            .parse()
            .map_err(Into::into)
    }

    fn has_attribute(&self, name: &str) -> bool {
        self.find_attribute(name).is_some()
    }

    fn find_attribute(&self, name: &str) -> Option<&Ast> {
        self.global_attributes
            .iter()
            .rev()
            .find(|a| a.token.value.deref() == name)
            .or(self
                .pushed_attributes
                .iter()
                .rev()
                .find(|a| a.token.value.deref() == name))
    }

    fn assert_atr_len(&self, attr: &Ast, expected: usize) -> Result<()> {
        if attr.len() < expected {
            Err(IrGenError::new(
                IGEKind::MissingAttrArgument(attr.len(), expected),
                attr.token.clone(),
            ))
        } else {
            Ok(())
        }
    }
}

pub struct Context {
    modules: Vec<Cell<Mod>>,
}

impl Context {
    fn new() -> Self {
        Self {
            modules: Vec::new(),
        }
    }

    fn add_module(&mut self, module: Cell<Mod>) -> Result<()> {
        match self
            .modules
            .binary_search_by(|d| module.name.cmp(&d.name))
        {
            Ok(i) => Err(IGEKind::DuplicateModule(module.clone(), self.modules[i].clone()).into()),
            Err(i) => {
                self.modules.insert(i, module);
                Ok(())
            }
        }
    }

    fn find_module(&self, name: Token) -> Result<Cell<Mod>> {
        match self
            .modules
            .binary_search_by(|d| name.value.cmp(&d.name))
        {
            Ok(i) => Ok(self.modules[i].clone()),
            Err(_) => Err(IrGenError::new(IGEKind::ModuleNotFound, name.clone())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mod {
    name: String,
    dependency: Vec<Cell<Mod>>,
    types: Vec<Cell<Datatype>>,
    functions: Vec<Cell<Fun>>,
}

impl Mod {
    fn new(name: String) -> Self {
        Self {
            name,
            dependency: vec![],
            types: vec![],
            functions: vec![],
        }
    }

    fn find_datatype(&self, name: &str) -> Option<Cell<Datatype>> {
        self.types
            .binary_search_by(|d| name.cmp(&d.name))
            .ok()
            .map(|i| self.types[i].clone())
            .or_else(|| {
                for dep in self.dependency.iter().rev() {
                    if let Some(d) = dep.find_datatype(name) {
                        return Some(d);
                    }
                }
                None
            })
    }

    fn add_datatype(&mut self, datatype: Cell<Datatype>) -> Result<()> {
        match self
            .types
            .binary_search_by(|d| datatype.name.cmp(&d.name))
        {
            Ok(i) => Err(IGEKind::DuplicateType(datatype.clone(), self.types[i].clone()).into()),
            Err(i) => {
                self.types.insert(i, datatype);
                Ok(())
            }
        }
    }

    fn has_function(&self, name: &str) -> bool {
        self.functions
            .binary_search_by(|d| name.cmp(&d.name))
            .is_ok()
    }

    fn add_function(&mut self, fun: Cell<Fun>) -> Result<()> {
        match self
            .functions
            .binary_search_by(|d| fun.name.cmp(&d.name))
        {
            Ok(i) => Err(IGEKind::DuplicateFunction(fun.clone(), self.functions[i].clone()).into()),
            Err(i) => {
                self.functions.insert(i, fun);
                Ok(())
            }
        }
    }

    fn find_function(&self, name: &Token) -> Option<Cell<Fun>> {
        self.functions
            .binary_search_by(|f| name.value.cmp(&f.name))
            .ok()
            .map(|i| self.functions[i].clone())
            .or_else(|| {
                for dep in self.dependency.iter().rev() {
                    if let Some(d) = dep.find_function(name) {
                        return Some(d);
                    }
                }
                None
            })
    }
}

macro_rules! builtin_repo {
    (types [$($name:ident: $lit:ident $bits:expr,)*]) => {
        pub struct BuiltinRepo {
            $($name: Cell<Datatype>,)*
        }

        impl BuiltinRepo {
            fn new() -> Self {
                Self {
                    $(
                        $name: Cell::new(Datatype::with_size(
                            stringify!($name).to_string(),
                            DKind::Builtin($lit),
                            $bits
                        )),
                    )*
                }
            }

            fn to_module(&self) -> Cell<Mod> {
                let mut module = Mod::new("builtin".to_string());
                $(
                    module.add_datatype(self.$name.clone()).unwrap();
                )*
                Cell::new(module)
            }
        }
    }
}

builtin_repo!(
    types [
        i8: I8 8,
        i16: I16 16,
        i32: I32 32,
        i64: I64 64,
        u8: I8 8,
        u16: I16 16,
        u32: I32 32,
        u64: I64 64,
        f32: F32 32,
        f64: F64 64,
        bool: B1 1,
    ]
);

#[derive(Debug, Clone)]
pub struct Fun {
    id: FuncId,
    name: StrRef,
    params: Vec<Var>,
    ret: Option<Cell<Datatype>>,
    inline_level: InlineLevel,
    signature: Option<Signature>,
    body: Ast,
}

#[derive(Debug, Clone)]
pub enum InlineLevel {
    Never,
    Auto,
    Always,
}

impl FromStr for InlineLevel {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "never" => Ok(InlineLevel::Never),
            "auto" => Ok(InlineLevel::Auto),
            "always" => Ok(InlineLevel::Always),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Var {
    name: StrRef,
    access: VarAccess,
    datatype: Cell<Datatype>,
}

impl Var {
    fn new(name: StrRef, access: VarAccess, datatype: Cell<Datatype>) -> Self {
        Self {
            datatype,
            name,
            access,
        }
    }
}

#[derive(Debug, Clone)]
pub enum VarAccess {
    Mutable(Variable),
    Immutable(Value),
    Unresolved,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Datatype {
    name: String,
    kind: DKind,
    size: usize,
}

impl Datatype {
    fn new(name: String, kind: DKind) -> Self {
        Self::with_size(name, kind, 0)
    }

    fn with_size(name: String, kind: DKind, size: usize) -> Self {
        Self { name, kind, size }
    }

    fn get_ir_repr(&self) -> Type {
        match self.kind {
            DKind::Builtin(tp) => tp,
            DKind::Pointer(_) => todo!(),
            DKind::Alias(_) => todo!(),
            DKind::Structure(_) => todo!(),
            DKind::Enum(_) => todo!(),
        }
    }

    fn default_value(&self, builder: &mut FunctionBuilder) -> Value {
        match self.kind {
            DKind::Builtin(tp) => match tp {
                I8 => builder.ins().iconst(I8, 0),
                I16 => builder.ins().iconst(I16, 0),
                I32 => builder.ins().iconst(I32, 0),
                I64 => builder.ins().iconst(I64, 0),
                F32 => builder.ins().f32const(0.0),
                F64 => builder.ins().f64const(0.0),
                B1 => builder.ins().bconst(B1, false),
                _ => panic!("unsupported builtin type"),
            },
            DKind::Pointer(_) => todo!(),
            DKind::Alias(_) => todo!(),
            DKind::Structure(_) => todo!(),
            DKind::Enum(_) => todo!(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DKind {
    Builtin(Type),
    Pointer(Cell<Datatype>),
    Alias(Cell<Datatype>),
    Structure(Structure),
    Enum(Enum),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Structure {}

#[derive(Debug, Clone, PartialEq)]
pub enum Enum {}

#[derive(Debug)]
pub struct IrGenError {
    kind: IGEKind,
    token: Option<Token>,
}

impl IrGenError {
    fn new(kind: IGEKind, token: Token) -> Self {
        Self {
            kind,
            token: Some(token),
        }
    }
}

impl Into<IrGenError> for AstError {
    fn into(self) -> IrGenError {
        IrGenError {
            kind: IGEKind::AstError(self.kind),
            token: self.token,
        }
    }
}

#[derive(Debug)]
pub enum IGEKind {
    TypeMismatch(Cell<Datatype>, Cell<Datatype>),
    DuplicateType(Cell<Datatype>, Cell<Datatype>),
    DuplicateModule(Cell<Mod>, Cell<Mod>),
    DuplicateFunction(Cell<Fun>, Cell<Fun>),
    MissingAttrArgument(usize, usize),
    InvalidAmountOfArguments(usize, usize),
    InvalidInlineLevel,
    InvalidLinkage,
    InvalidCallConv,
    AssignToImmutable,
    ExpectedValue,
    MissingValueInElseBranch,
    UnexpectedValueInThenBranch,
    UnexpectedValue,
    LoopHeaderNotFound,
    MissingElseBranch,
    FunctionNotFound,
    VariableNotFound,
    TypeNotFound,
    ModuleNotFound,
    CannotOpenFile(std::io::Error),
    AstError(AEKind),
}

impl Into<IrGenError> for IGEKind {
    fn into(self) -> IrGenError {
        IrGenError {
            kind: self,
            token: None,
        }
    }
}

impl<'a> SealCurrentBlock for FunctionBuilder<'a> {
    fn seal_current_block(&mut self) {
        self.seal_block(self.current_block().unwrap());
    }
}

trait SealCurrentBlock {
    fn seal_current_block(&mut self);
}

pub struct Cell<T> {
    inner: *mut (T, usize),
}

impl<T> Cell<T> {
    fn new(inner: T) -> Self {
        Self {
            inner: Box::into_raw(Box::new((inner, 1))),
        }
    }
}

impl<T: PartialEq> PartialEq for Cell<T> {
    fn eq(&self, other: &Self) -> bool {
        unsafe {
            if self.inner == other.inner {
                return true;
            }
            let (a, _a_count) = &*self.inner;
            let (b, _b_count) = &*other.inner;
            *a == *b
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Cell<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", unsafe { &*self.inner })
    }
}

impl<T> Clone for Cell<T> {
    fn clone(&self) -> Self {
        unsafe {
            (*self.inner).1 += 1;
            Self {
                inner: self.inner,
            }
        }
    }
}

impl<T> Drop for Cell<T> {
    fn drop(&mut self) {
        unsafe {
            (*self.inner).1 -= 1;
            if (*self.inner).1 == 0 {
                Box::from_raw(self.inner);
            }
        }
    }
}

impl<T> DerefMut for Cell<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut(*self.inner).0 }
    }
}

impl<T> Deref for Cell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.inner).0 }
    }
}