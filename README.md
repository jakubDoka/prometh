# MetaFlow

## Mindset

(All you see is more planned then implemented.)

MetaFlow, as its name tells, is focused on complex metaprogramming. This means you can write code to generate code (to generate code...), make DSL-s (Domain Specific Language) that can be used by other people by just including your package in dependencies. Features include Generics, Templates and String/Token/Ast/IR macros. Though Templates will be just macros generated by macro from standard library. This is no easy task, fortunately Metaflow uses Cranelift backend so all macros can be jit compiled. Garbage collector is not built into language but can be created with macros. Idea is to make macro analyze a datatype and generate code that traverses RC and ARC references to find cycles and solve other problems.
This makes garbage collector optional and it is more likely, standard library will not rely on it.

Metaflow is not object oriented and it does not support reflection as a default feature. On the other hand it allows user simplify the mess in other ways. Lets demonstrate.

```mf

struct Counter:
  count: int

fun inc(c: &Counter):
  ++c.count

struct Something:
  embed counter: Counter

attr entry
fun main -> int:
  var s: Something

  // subtract 12
  s.counter.count -= 2
  s.count -= 10

  loop:
    if s.count < 0:
      break
    // increment 3
    inc(&s.counter)
    s.counter.inc()
    s.inc()

  // return 0
  return s.count
```

Each type can embed multiple fields and field and it works recursively. Every function can be called in dot style on the first argument. Though you still can use `pub` and `priv` to restrict use of code items and fields. This is merely to give authors more tools to create friendly API for their libraries.

MetaFlow is space significant language. Indentation matters, semicolon isn't even recognized by lexer (yet).

Metaflow supports static dispatch. You can define method with same name and as long as it has different arguments, it will compile. This does not include macros though.

Metaflow is mainly inspired by Nim, Go and Rust. Compiler is written in Rust and it probably will stay that way.

### Limitations

There are just two simple restriction regarding macros. All functions macro calls must be marked for jit compilation. Macro cannot be called from module where it is defined.

### Macro Limitation Explained

Firstly, order of definition does not matter so it is impossible to make code generated by macro from same file work properly. Secondly, to lower compiler memory consumption and reuse allocations, compiler compiles module at a time and recycles ast tree that is no longer needed. It also does not cache intermediate representation not the Cranelift intermediate representation. Memory would blow up if we saved everything for potential jit compilations. On the other hand, jit compiling everything is also not optimal.

## Documentation

This section documents most of the properties of MetaFlow

### Source organization

Compiler considers files as separate modules. Whole project including project manifest is considered a Package. Packages can be imported in manifest (section 'dependencies'). Cyclic dependency either between packages or modules is no allowed. This allows compiler to compile more efficiently and over all makes code easier to traverse as no two files reference each other.

### Definition order

Order of definitions within module does not matter but general practice (i just declared) is writing declaration after use when possible for any code item. This does not include global 'var' and 'let', they should be on the top of the file right after 'use'. Mentioned 'use' keyword can be used only once per file and must be first. Other occurrences of 'use' will result in error.

### Order of compilation

Compilation order is stable and controllable. If you want to determinate order of compilation, for example you have modules A -> [B, C] -> D and you want the C to be compiled after B because they change globals state in D during compile time. This can be achieved by making C -> B.

### Dependency management

Dependency management is done trough git. Its really simple actually. In manifest you specify:

```mf
dependencies:
  something "github.come/someone/something@tag"
```

You can now 'use' modules from `something` as:

```mf
use
  "something"
  alias "something/submodule"
```

You can refer to items from module as `<alias or module file name>::item` but this is only last resort as you can omit this if there is no name collision. Alias is also optional as by default, last segment of path is used as alias.

Package structure is determined by manifest but also some implementation details. (i am too lazy to solve...). You have to specify the 'root' attribute with a name of root module and you have to place submodules into directory with the same name as the root module. For example:

```txt
package.mfm // contains: root = "src/main.mf"
src/
  main.mf
  main/
    sub.mf
    other_sub.mf
```

Why is it like this? Well, dependencies can be aliased and so first segment of the module path has to be disregarded and replaced with the root file name. Thus simply, first path segment is always the root name.

You can control where are dependencies placed by using `METAFLOW_CACHE`. This simple batch file will prepare environment and compile the project:

```bat
@rem sets cache to directory local to project, generally 
@rem you want to save space and use global cache
set METAFLOW_CACHE=deps
@rem calls the compiler specifying directory with manifest
mf .
```

### Syntax

The syntax is expressed by yet another syntax. Words between `'` are keywords or punctuation, things between `[]` are optional, things between `{}` are required, `|` denotes choice, `=` denotes alias and `:` optional indented block, thing between `"` is regex. Lets start.

```txt
file = [ use '\n' ] { item '\n' }
use = 'use' : [ ident ] string

item = 
  function | 
  struct
function = 'fun' [ vis ] ident | op [ generics ] [ args ] [ '->' type ] [ ':' : statement ]
struct = 'struct' [ vis ] ident [ generics ] [ ':' : field ]
field = [ vis ] [ 'embed' ] ident { ',' ident } ':' type

statement =
  if_stmt | 
  loop_stmt | 
  break_stmt | 
  continue_stmt | 
  return_stmt | 
  var_stmt | 
  expr
if_stmt = 'if' expr ':' : statement { elif ':' : statement } [ 'else' ':' : statement ]
loop_stmt = 'loop' [ label ] ':' : statement
break_stmt = 'break' [ label ] [ expr ]
continue_stmt = 'continue' [ label ]
return_stmt = 'return' [ expr ]
var_stmt = 'var' | 'let' : ident { ',' ident } [ ':' type ] [ '=' expr { ',' expr } ]

expr = 
  literal | 
  call | 
  access | 
  assign | 
  binary | 
  unary | 
  cast | 
  ref_expr | 
  deref_expr
call = [expr '.'] ident '(' [ expr { ',' expr } ] ')'
access = [expr '.'] ident
assign = expr '=' expr
binary = expr op expr
unary = op expr
cast = expr 'as' type
ref_expr = ref expr
deref_expr = '*' expr

literal =
  number |
  string |
  bool |
  char |
number = "\d+\.?\d*[iuf]\d{0, 2}"
string = "\"[\s\S]*\""
bool = 'true' | 'false'
char = "'\p{L}|\p{N}|\\\d{3}|\\x[0-9a-fA-F]{2}|\\u[0-9a-fA-F]{4}|\\U[0-9a-fA-F]{8}'"

type = [ ref ] ident [ '[' type { ',' type } ']' ]
ref = '&' [ var ]
generics = '[' ident { ',' ident } ']'
args = '(' { [ 'var' ] ident { ',' ident } ':' type } ')'
vis = 'pub' | 'priv'
label = "'[a-zA-Z0-9_]+"
op = "[\+\-\*/%\^=<>!&|\?:~]+|min|max|abs"
ident = "[a-zA-Z_][a-zA-Z0-9_]+"

```

### To be continued

## Compiler design

This section merely describes how compiler works as a reference for me. Things you see may be planned in a future, but are written in present tense.

### Memory management and access

Almost all the data compiler uses during compilation is stored in constructs contained in `crate::util::storage`. Each ordered container has an accessor type that implements `crate::util::storage::IndexPointer`. Every entity has its Index type that has a descriptive name and data itself is in `<name>Ent` struct. This is safe and convenient way of creating complex structures like graphs without ref count and over all makes borrow checker happy.

Exception to this rule is `crate::ast::Ast` which does not use this kind of storage for sanity reasons, it also does not need to as its only used as immutable structure.

What you will see a lot is `self.context.pool.get()` whenever temporary Vec is needed. Pool saves used Vec-s and only allocate if there is not vec to reuse. What pool returns is PoolRef that will send it self to pool upon drop.

Important decision was leaking the memory of source files. This makes them static and allows tokens capture whole file without lifetime. The file contents has to live whole duration of program anyway. Though, if this approach causes issues in a future, rework will be inevitable. With nowadays RAM sizes it should be fine though.

### Compilation steps

Compiler divides compilation into several steps to make logic manageable for human being. FIrst is a lexer which is an iterator that yields tokens lazily. Tokens then pass trough AstParser that constructs the ast which is then passed to Type parser. Type parser collects all declarations of types and passes remaining ast to function parser. This parser creates simple intermediate representations. At the end, th representation is passed to code generator that translates ir to Cranelift ir. Cranelift does the rest. To lower peak memory and reuse allocations, this is performed per module. Modules are sorted so that no module precedes its dependencies.
