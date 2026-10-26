use crate::editor::{Editor, SnippetTabStop};

#[cfg(test)]
#[path = "editor_snippet_catalog_tests.rs"]
mod catalog_tests;

// Keep lookup and its scan bound derived from the same trigger definitions.
// Ordinary Tab can reject a giant identifier after a handful of characters;
// adding a longer snippet automatically updates the bound for that language.
macro_rules! define_snippets {
    ($( $($lang:literal)|+ => {
        $( $($trigger:literal)|+ => $body:expr, )*
        _ => None,
    }, )* _ => None,) => {
        pub(crate) fn get_snippet(trigger: &str, lang: &str) -> Option<&'static str> {
            match lang {
                $( $($lang)|+ => match trigger { $( $($trigger)|+ => $body, )* _ => None }, )*
                _ => None,
            }
        }

        pub(crate) fn snippet_triggers(lang: &str) -> &'static [&'static str] {
            match lang { $( $($lang)|+ => &[$( $( $trigger, )+ )*], )* _ => &[] }
        }

        fn max_snippet_trigger_bytes(lang: &str) -> usize {
            match lang {
                $( $($lang)|+ => {
                    const MAX: usize = {
                        let mut max = 0;
                        $( $( if $trigger.len() > max { max = $trigger.len(); } )+ )*
                        max
                    };
                    MAX
                }, )*
                _ => 0,
            }
        }

        #[cfg(test)]
        const SNIPPET_LANGUAGE_GROUPS: &[&[&str]] = &[$( &[$($lang),+], )*];
    };
}

define_snippets! {
        "Rust" => {
            "fn" => Some("fn ${1:name}($2) {\n    $0\n}"),
            "pfn" => Some("pub(crate) fn ${1:name}($2) {\n    $0\n}"),
            "struct" => Some("struct ${1:Name} {\n    $0\n}"),
            "enum" => Some("enum ${1:Name} {\n    $0\n}"),
            "impl" => Some("impl ${1:Type} {\n    $0\n}"),
            "test" => Some("#[test]\nfn test_${1:name}() {\n    $0\n}"),
            "match" => Some("match ${1:expr} {\n    ${2:_} => {$0}\n}"),
            "iflet" => Some("if let Some(${1:val}) = ${2:option} {\n    $0\n}"),
            "loop" => Some("loop {\n    $0\n}"),
            "derive" => Some("#[derive($0)]"),
            _ => None,
        },
        "JavaScript" | "JavaScript (Plain)" | "JavaScript React" | "JSX" | "JSX (Plain)" | "TypeScript" | "TypeScript (Plain)" | "TypeScript React" | "TSX" | "TSX (Plain)" | "JavaScript (Rails)" => {
            "fn" | "func" => Some("function ${1:name}($2) {\n    $0\n}"),
            "afn" => Some("async function ${1:name}($2) {\n    $0\n}"),
            "arrow" => Some("const ${1:name} = ($2) => {\n    $0\n};"),
            "class" => Some("class ${1:Name} {\n    constructor($2) {\n        $0\n    }\n}"),
            "for" => Some("for (let i = ${1:0}; i < ${2:length}; i++) {\n    $0\n}"),
            "fore" => Some("for (const ${1:item} of ${2:items}) {\n    $0\n}"),
            "if" => Some("if (${1:condition}) {\n    $0\n}"),
            "ife" => Some("if (${1:condition}) {\n    $2\n} else {\n    $0\n}"),
            "try" => Some("try {\n    $1\n} catch (${2:error}) {\n    $0\n}"),
            "log" => Some("console.log($0);"),
            "imp" => Some("import { $2 } from '${1:module}';$0"),
            _ => None,
        },
        "Python" => {
            "def" => Some("def ${1:name}($2):\n    ${0:pass}"),
            "adef" => Some("async def ${1:name}($2):\n    ${0:pass}"),
            "class" => Some("class ${1:Name}:\n    def __init__(self$2):\n        ${0:pass}"),
            "if" => Some("if ${1:condition}:\n    ${0:pass}"),
            "ife" => Some("if ${1:condition}:\n    ${2:pass}\nelse:\n    ${0:pass}"),
            "for" => Some("for ${1:item} in ${2:items}:\n    ${0:pass}"),
            "try" => Some("try:\n    ${1:pass}\nexcept ${2:Exception} as ${3:e}:\n    ${0:pass}"),
            "with" => Some("with open('${1:file}') as ${2:f}:\n    ${0:pass}"),
            "main" => Some("if __name__ == '__main__':\n    ${0:pass}"),
            _ => None,
        },
        "HTML" | "HTML (Rails)" | "HTML (Tcl)" => {
            // Emmet-style abbreviations
            "html5" | "doc" => Some("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n    <meta charset=\"UTF-8\">\n    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n    <title>Document</title>\n</head>\n<body>\n    \n</body>\n</html>"),
            "div" => Some("<div>\n    \n</div>"),
            "span" => Some("<span></span>"),
            "p" => Some("<p></p>"),
            "a" => Some("<a href=\"\"></a>"),
            "img" => Some("<img src=\"\" alt=\"\" />"),
            "ul" => Some("<ul>\n    <li></li>\n</ul>"),
            "ol" => Some("<ol>\n    <li></li>\n</ol>"),
            "li" => Some("<li></li>"),
            "table" => Some("<table>\n    <thead>\n        <tr>\n            <th></th>\n        </tr>\n    </thead>\n    <tbody>\n        <tr>\n            <td></td>\n        </tr>\n    </tbody>\n</table>"),
            "form" => Some("<form action=\"\" method=\"post\">\n    \n</form>"),
            "input" => Some("<input type=\"text\" name=\"\" id=\"\" />"),
            "btn" | "button" => Some("<button type=\"button\"></button>"),
            "select" => Some("<select name=\"\" id=\"\">\n    <option value=\"\"></option>\n</select>"),
            "textarea" => Some("<textarea name=\"\" id=\"\" cols=\"30\" rows=\"10\"></textarea>"),
            "nav" => Some("<nav>\n    \n</nav>"),
            "header" => Some("<header>\n    \n</header>"),
            "footer" => Some("<footer>\n    \n</footer>"),
            "main" => Some("<main>\n    \n</main>"),
            "section" => Some("<section>\n    \n</section>"),
            "article" => Some("<article>\n    \n</article>"),
            "aside" => Some("<aside>\n    \n</aside>"),
            "script" => Some("<script>\n    \n</script>"),
            "style" => Some("<style>\n    \n</style>"),
            "link" => Some("<link rel=\"stylesheet\" href=\"\" />"),
            "meta" => Some("<meta name=\"\" content=\"\" />"),
            _ => None,
        },
        "CSS" | "SCSS" | "Less" | "CSS (PHP)" => {
            "bg" | "bgc" => Some("background-color: ;"),
            "bgi" => Some("background-image: url();"),
            "bd" => Some("border: 1px solid ;"),
            "br" => Some("border-radius: ;"),
            "m" => Some("margin: ;"),
            "p" => Some("padding: ;"),
            "d" => Some("display: ;"),
            "df" => Some("display: flex;"),
            "dg" => Some("display: grid;"),
            "dn" => Some("display: none;"),
            "db" => Some("display: block;"),
            "dib" => Some("display: inline-block;"),
            "jc" => Some("justify-content: ;"),
            "ai" => Some("align-items: ;"),
            "fd" => Some("flex-direction: ;"),
            "fw" => Some("font-weight: ;"),
            "fs" => Some("font-size: ;"),
            "ff" => Some("font-family: ;"),
            "w" => Some("width: ;"),
            "h" => Some("height: ;"),
            "mw" => Some("max-width: ;"),
            "mh" => Some("max-height: ;"),
            "pos" => Some("position: ;"),
            "t" => Some("top: ;"),
            "l" => Some("left: ;"),
            "r" => Some("right: ;"),
            "b" => Some("bottom: ;"),
            "c" => Some("color: ;"),
            "ta" => Some("text-align: ;"),
            "td" => Some("text-decoration: ;"),
            "tt" => Some("text-transform: ;"),
            "lh" => Some("line-height: ;"),
            "ls" => Some("letter-spacing: ;"),
            "z" => Some("z-index: ;"),
            "o" => Some("opacity: ;"),
            "ov" => Some("overflow: ;"),
            "trs" => Some("transition: all 0.3s ease;"),
            "ani" => Some("animation: name 0.3s ease;"),
            "bs" => Some("box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);"),
            _ => None,
        },
        "C" | "CMake C Header" => {
            "main" => Some("int main(int argc, char *argv[]) {\n    $0\n    return 0;\n}"),
            "fn" | "func" => Some("${1:void} ${2:name}($3) {\n    $0\n}"),
            "if" => Some("if (${1:condition}) {\n    $0\n}"),
            "ife" => Some("if (${1:condition}) {\n    $2\n} else {\n    $0\n}"),
            "for" => Some("for (int i = 0; i < ${1:count}; ++i) {\n    $0\n}"),
            "while" => Some("while (${1:condition}) {\n    $0\n}"),
            "do" => Some("do {\n    $0\n} while (${1:condition});"),
            "switch" => Some("switch (${1:expression}) {\ncase ${2:value}:\n    $0\n    break;\ndefault:\n    break;\n}"),
            "case" => Some("case ${1:value}:\n    $0\n    break;"),
            "struct" => Some("struct ${1:Name} {\n    ${0:int member;}\n};"),
            "typedef" => Some("typedef ${1:type} ${2:Name};$0"),
            "tstruct" => Some("typedef struct {\n    ${2:int member;}\n} ${1:Name};$0"),
            "enum" => Some("enum ${1:Name} {\n    ${0:VALUE}\n};"),
            "inc" | "include" => Some("#include <${1:stdio.h}>\n$0"),
            "incl" => Some("#include \"${1:header.h}\"\n$0"),
            "define" => Some("#define ${1:NAME} ${2:value}\n$0"),
            "ifdef" => Some("#ifdef ${1:NAME}\n$0\n#endif"),
            "once" => Some("#pragma once\n\n$0"),
            "printf" => Some("printf(\"${1:message}\\n\"$2);$0"),
            "snprintf" => Some("snprintf(${1:buffer}, ${2:size}, \"${3:format}\"$4);$0"),
            "ret" => Some("return ${1:value};$0"),
            "assert" => Some("assert(${1:condition});$0"),
            _ => None,
        },
        "C++" | "C/C++ Header" | "CMake C++ Header" => {
            "main" => Some("int main(int argc, char *argv[]) {\n    $0\n    return 0;\n}"),
            "fn" | "func" => Some("${1:void} ${2:name}($3) {\n    $0\n}"),
            "if" => Some("if (${1:condition}) {\n    $0\n}"),
            "ife" => Some("if (${1:condition}) {\n    $2\n} else {\n    $0\n}"),
            "for" => Some("for (int i = 0; i < ${1:count}; ++i) {\n    $0\n}"),
            "fore" => Some("for (const auto& ${1:item} : ${2:items}) {\n    $0\n}"),
            "while" => Some("while (${1:condition}) {\n    $0\n}"),
            "do" => Some("do {\n    $0\n} while (${1:condition});"),
            "switch" => Some("switch (${1:expression}) {\ncase ${2:value}:\n    $0\n    break;\ndefault:\n    break;\n}"),
            "class" => Some("class ${1:Name} {\npublic:\n    $0\n};"),
            "struct" => Some("struct ${1:Name} {\n    $0\n};"),
            "enum" => Some("enum class ${1:Name} {\n    ${0:Value}\n};"),
            "namespace" | "ns" => Some("namespace ${1:name} {\n    $0\n}"),
            "template" => Some("template <typename ${1:T}>\n$0"),
            "lambda" => Some("auto ${1:name} = [${2:&}]($3) {\n    $0\n};"),
            "try" => Some("try {\n    $1\n} catch (const ${2:std::exception}& ${3:error}) {\n    $0\n}"),
            "inc" | "include" => Some("#include <${1:iostream}>\n$0"),
            "incl" => Some("#include \"${1:header.hpp}\"\n$0"),
            "once" => Some("#pragma once\n\n$0"),
            "using" => Some("using ${1:Name} = ${2:Type};$0"),
            "cout" => Some("std::cout << ${1:value} << '\\n';$0"),
            "unique" => Some("auto ${1:name} = std::make_unique<${2:Type}>($3);$0"),
            "shared" => Some("auto ${1:name} = std::make_shared<${2:Type}>($3);$0"),
            "vector" => Some("std::vector<${1:Type}> ${2:items};$0"),
            "ret" => Some("return ${1:value};$0"),
            "assert" => Some("assert(${1:condition});$0"),
            _ => None,
        },
        "C#" | "CSharp" | "csharp" => {
            "class" => Some("public class ${1:ClassName}\n{\n    $0\n}"),
            "struct" => Some("public struct ${1:Name}\n{\n    $0\n}"),
            "interface" => Some("public interface ${1:IName}\n{\n    $0\n}"),
            "enum" => Some("public enum ${1:Name}\n{\n    ${0:Value}\n}"),
            "record" => Some("public record ${1:Name}($2);$0"),
            "namespace" | "ns" => Some("namespace ${1:Name}\n{\n    $0\n}"),
            "using" => Some("using ${1:System};\n$0"),
            "fn" | "method" => Some("private ${1:void} ${2:Method}($3)\n{\n    $0\n}"),
            "afn" => Some("private async Task ${1:MethodAsync}($2)\n{\n    $0\n}"),
            "ctor" => Some("public ${1:ClassName}($2)\n{\n    $0\n}"),
            "prop" => Some("public ${1:string} ${2:Name} { get; set; }$0"),
            "propg" => Some("public ${1:int} ${2:Value} { get; }$0"),
            "main" => Some("static void Main(string[] args)\n{\n    $0\n}"),
            "if" => Some("if (${1:condition})\n{\n    $0\n}"),
            "ife" => Some("if (${1:condition})\n{\n    $2\n}\nelse\n{\n    $0\n}"),
            "for" => Some("for (int i = 0; i < ${1:count}; i++)\n{\n    $0\n}"),
            "fore" | "foreach" => Some("foreach (var ${1:item} in ${2:items})\n{\n    $0\n}"),
            "while" => Some("while (${1:condition})\n{\n    $0\n}"),
            "switch" => Some("switch (${1:expression})\n{\n    case ${2:value}:\n        $0\n        break;\n    default:\n        break;\n}"),
            "try" => Some("try\n{\n    $1\n}\ncatch (${2:Exception} ${3:ex})\n{\n    $0\n}"),
            "lock" => Some("lock (${1:gate})\n{\n    $0\n}"),
            "cw" | "log" => Some("Console.WriteLine($0);"),
            _ => None,
        },
        "Lua" => {
            "fn" | "function" => Some("function ${1:name}($2)\n    $0\nend"),
            "lfn" => Some("local function ${1:name}($2)\n    $0\nend"),
            "if" => Some("if ${1:condition} then\n    $0\nend"),
            "ife" => Some("if ${1:condition} then\n    $2\nelse\n    $0\nend"),
            "elseif" => Some("elseif ${1:condition} then\n    $0"),
            "for" => Some("for ${1:i} = ${2:1}, ${3:count} do\n    $0\nend"),
            "forp" => Some("for ${1:key}, ${2:value} in pairs(${3:items}) do\n    $0\nend"),
            "fori" => Some("for ${1:index}, ${2:value} in ipairs(${3:items}) do\n    $0\nend"),
            "while" => Some("while ${1:condition} do\n    $0\nend"),
            "repeat" => Some("repeat\n    $0\nuntil ${1:condition}"),
            "table" => Some("local ${1:name} = {\n    $0\n}"),
            "local" => Some("local ${1:name} = ${2:value}$0"),
            "req" | "require" => Some("local ${1:module} = require(\"${2:module}\")$0"),
            "print" => Some("print($0)"),
            "ret" => Some("return ${1:value}$0"),
            "pcall" => Some("local ${1:ok}, ${2:result} = pcall(${3:fn}$4)\n$0"),
            _ => None,
        },
        "Tcl" | "TCL" => {
            "proc" => Some("proc ${1:name} {${2:args}} {\n    $0\n}"),
            "set" => Some("set ${1:name} ${2:value}$0"),
            "if" => Some("if {${1:1}} {\n    $0\n}"),
            "ife" => Some("if {${1:1}} {\n    $2\n} else {\n    $0\n}"),
            "foreach" | "fore" => Some("foreach ${1:item} \\$${2:items} {\n    $0\n}"),
            "for" => Some("for {set i 0} {\\$i < ${1:10}} {incr i} {\n    $0\n}"),
            "while" => Some("while {${1:0}} {\n    $0\n}"),
            "switch" => Some("switch -- \\$${1:value} {\n    ${2:pattern} { ${3:# body} }\n    default { $0 }\n}"),
            "catch" => Some("if {[catch {\n    ${1:# command}\n} ${2:result}]} {\n    $0\n}"),
            "namespace" => Some("namespace eval ${1:name} {\n    $0\n}"),
            "package" => Some("package require ${1:package}$0"),
            "dict" => Some("dict set ${1:dictionary} ${2:key} ${3:value}$0"),
            "dictget" => Some("dict get \\$${1:dictionary} ${2:key}$0"),
            "dictfor" => Some("dict for {${1:key} ${2:value}} \\$${3:dictionary} {\n    $0\n}"),
            "list" => Some("set ${1:items} [list $0]"),
            "lappend" => Some("lappend ${1:items} ${2:value}$0"),
            "puts" => Some("puts \"${1:message}\"$0"),
            "expr" => Some("expr {${1:1 + 1}}$0"),
            "source" => Some("source [file join [file dirname [info script]] ${1:file.tcl}]\n$0"),
            _ => None,
        },
        "Go" => {
            "main" => Some("func main() {\n    $0\n}"),
            "fn" | "func" => Some("func ${1:name}($2) {\n    $0\n}"),
            "fne" => Some("func ${1:name}($2) error {\n    ${0:return nil}\n}"),
            "method" => Some("func (${1:receiver} *${2:Type}) ${3:Name}($4) {\n    $0\n}"),
            "if" => Some("if ${1:condition} {\n    $0\n}"),
            "ife" => Some("if ${1:condition} {\n    $2\n} else {\n    $0\n}"),
            "iferr" => Some("if err != nil {\n    ${0:return err}\n}"),
            "for" => Some("for i := 0; i < ${1:count}; i++ {\n    $0\n}"),
            "range" | "fore" => Some("for ${1:key}, ${2:value} := range ${3:items} {\n    $0\n}"),
            "switch" => Some("switch ${1:value} {\ncase ${2:pattern}:\n    $0\ndefault:\n}"),
            "struct" => Some("type ${1:Name} struct {\n    $0\n}"),
            "interface" => Some("type ${1:Name} interface {\n    $0\n}"),
            "imp" => Some("import \"${1:package}\"\n$0"),
            "package" => Some("package ${1:main}\n\n$0"),
            "test" => Some("func Test${1:Name}(t *testing.T) {\n    $0\n}"),
            "defer" => Some("defer ${1:fn}($2)\n$0"),
            "goroutine" => Some("go func() {\n    $0\n}()"),
            "select" => Some("select {\ncase ${1:value} := <-${2:channel}:\n    $0\n}"),
            _ => None,
        },
        "Java" => {
            "class" => Some("public class ${1:ClassName} {\n    $0\n}"),
            "interface" => Some("public interface ${1:Name} {\n    $0\n}"),
            "enum" => Some("public enum ${1:Name} {\n    ${0:VALUE}\n}"),
            "record" => Some("public record ${1:Name}($2) {}$0"),
            "main" => Some("public static void main(String[] args) {\n    $0\n}"),
            "fn" | "method" => Some("public ${1:void} ${2:name}($3) {\n    $0\n}"),
            "ctor" => Some("public ${1:ClassName}($2) {\n    $0\n}"),
            "if" => Some("if (${1:condition}) {\n    $0\n}"),
            "ife" => Some("if (${1:condition}) {\n    $2\n} else {\n    $0\n}"),
            "for" => Some("for (int i = 0; i < ${1:count}; i++) {\n    $0\n}"),
            "fore" => Some("for (${1:Type} ${2:item} : ${3:items}) {\n    $0\n}"),
            "while" => Some("while (${1:condition}) {\n    $0\n}"),
            "switch" => Some("switch (${1:expression}) {\n    case ${2:value}:\n        $0\n        break;\n    default:\n        break;\n}"),
            "try" => Some("try {\n    $1\n} catch (${2:Exception} ${3:ex}) {\n    $0\n}"),
            "tryr" => Some("try (${1:resource}) {\n    $0\n}"),
            "sout" | "log" => Some("System.out.println($0);"),
            "imp" => Some("import ${1:package};\n$0"),
            "package" => Some("package ${1:name};\n\n$0"),
            _ => None,
        },
        "Ruby" | "Ruby (Rails)" => {
            "def" => Some("def ${1:name}($2)\n  $0\nend"),
            "init" => Some("def initialize($1)\n  $0\nend"),
            "class" => Some("class ${1:Name}\n  $0\nend"),
            "module" => Some("module ${1:Name}\n  $0\nend"),
            "if" => Some("if ${1:condition}\n  $0\nend"),
            "ife" => Some("if ${1:condition}\n  $2\nelse\n  $0\nend"),
            "unless" => Some("unless ${1:condition}\n  $0\nend"),
            "case" => Some("case ${1:value}\nwhen ${2:pattern}\n  $0\nelse\nend"),
            "each" => Some("${1:items}.each do |${2:item}|\n  $0\nend"),
            "map" => Some("${1:items}.map do |${2:item}|\n  $0\nend"),
            "while" => Some("while ${1:condition}\n  $0\nend"),
            "begin" => Some("begin\n  $1\nrescue ${2:StandardError} => ${3:error}\n  $0\nend"),
            "req" | "require" => Some("require \"${1:library}\"\n$0"),
            "attr" => Some("attr_accessor :${1:name}$0"),
            "puts" => Some("puts ${1:value}$0"),
            "test" => Some("def test_${1:name}\n  $0\nend"),
            _ => None,
        },
        "PHP" => {
            "fn" | "func" => Some("function ${1:name}($2): ${3:void} {\n    $0\n}"),
            "method" => Some("public function ${1:name}($2): ${3:void} {\n    $0\n}"),
            "ctor" => Some("public function __construct($1) {\n    $0\n}"),
            "class" => Some("class ${1:Name} {\n    $0\n}"),
            "interface" => Some("interface ${1:Name} {\n    $0\n}"),
            "trait" => Some("trait ${1:Name} {\n    $0\n}"),
            "namespace" => Some("namespace ${1:Name};\n\n$0"),
            "use" => Some("use ${1:Name};\n$0"),
            "if" => Some("if (${1:condition}) {\n    $0\n}"),
            "ife" => Some("if (${1:condition}) {\n    $2\n} else {\n    $0\n}"),
            "for" => Some("for (\\$i = 0; \\$i < \\$${1:count}; \\$i++) {\n    $0\n}"),
            "fore" | "foreach" => Some("foreach (\\$${1:items} as \\$${2:item}) {\n    $0\n}"),
            "while" => Some("while (${1:condition}) {\n    $0\n}"),
            "try" => Some("try {\n    $1\n} catch (\\Throwable \\$${2:error}) {\n    $0\n}"),
            "echo" => Some("echo ${1:value};$0"),
            "ret" => Some("return ${1:value};$0"),
            "array" => Some("[\n    $0\n]"),
            _ => None,
        },
        "Shell Script" | "Shell" | "Bash" | "Zsh" | "sh" | "bash" | "zsh" => {
            "fn" | "function" => Some("${1:name}() {\n    ${0::}\n}"),
            "if" => Some("if ${1:command}; then\n    ${0::}\nfi"),
            "ife" => Some("if ${1:command}; then\n    ${2::}\nelse\n    ${0::}\nfi"),
            "for" => Some("for ${1:item} in ${2:items}; do\n    ${0::}\ndone"),
            "while" => Some("while ${1:command}; do\n    ${0::}\ndone"),
            "until" => Some("until ${1:command}; do\n    ${0::}\ndone"),
            "case" => Some("case \"\\$${1:value}\" in\n    ${2:pattern})\n        ${0::}\n        ;;\nesac"),
            "read" => Some("while IFS= read -r ${1:line}; do\n    ${0::}\ndone < \"${2:file}\""),
            "var" => Some("${1:name}=\"${2:value}\"$0"),
            "export" => Some("export ${1:NAME}=\"${2:value}\"$0"),
            "printf" => Some("printf '%s\\n' \"${1:value}\"$0"),
            "source" => Some(". \"${1:file.sh}\"\n$0"),
            "trap" => Some("trap '${1:cleanup}' ${2:EXIT}\n$0"),
            "shebang" => Some("#!/bin/sh\n\n$0"),
            "arg" => Some("\"\\$${1:1}\"$0"),
            _ => None,
        },
        "Fish" | "fish" => {
            "fn" | "function" => Some("function ${1:name}\n    $0\nend"),
            "if" => Some("if ${1:command}\n    $0\nend"),
            "ife" => Some("if ${1:command}\n    $2\nelse\n    $0\nend"),
            "for" => Some("for ${1:item} in \\$${2:items}\n    $0\nend"),
            "while" => Some("while ${1:command}\n    $0\nend"),
            "switch" => Some("switch \\$${1:value}\n    case ${2:pattern}\n        $0\nend"),
            "set" => Some("set -l ${1:name} ${2:value}$0"),
            "export" => Some("set -gx ${1:NAME} ${2:value}$0"),
            "source" => Some("source ${1:file.fish}\n$0"),
            "shebang" => Some("#!/usr/bin/env fish\n\n$0"),
            _ => None,
        },
        "PowerShell" | "Powershell" | "powershell" => {
            "fn" | "function" => Some("function ${1:Get-Thing} {\n    param(\n        [${2:string}]\\$${3:Name}\n    )\n    $0\n}"),
            "param" => Some("param(\n    [${1:string}]\\$${2:Name}\n)\n$0"),
            "if" => Some("if (${1:condition}) {\n    $0\n}"),
            "ife" => Some("if (${1:condition}) {\n    $2\n} else {\n    $0\n}"),
            "for" => Some("for (\\$i = 0; \\$i -lt ${1:10}; \\$i++) {\n    $0\n}"),
            "fore" | "foreach" => Some("foreach (\\$${1:item} in \\$${2:items}) {\n    $0\n}"),
            "while" => Some("while (${1:condition}) {\n    $0\n}"),
            "switch" => Some("switch (\\$${1:value}) {\n    ${2:pattern} { $0 }\n}"),
            "try" => Some("try {\n    $1\n} catch {\n    $0\n}"),
            "tryf" => Some("try {\n    $1\n} finally {\n    $0\n}"),
            "pipeline" => Some("${1:command} | ForEach-Object {\n    $0\n}"),
            "where" => Some("Where-Object { ${1:condition} }$0"),
            "var" => Some("\\$${1:name} = ${2:value}$0"),
            "write" => Some("Write-Output ${1:value}$0"),
            "verbose" => Some("Write-Verbose \"${1:message}\"$0"),
            "hashtable" => Some("@{\n    ${1:Name} = ${2:value}\n    $0\n}"),
            _ => None,
        },
        "Kotlin" => {
            "fn" | "fun" => Some("fun ${1:name}($2): ${3:Unit} {\n    $0\n}"),
            "main" => Some("fun main() {\n    $0\n}"),
            "class" => Some("class ${1:Name} {\n    $0\n}"),
            "data" => Some("data class ${1:Name}(${2:val value: String})$0"),
            "interface" => Some("interface ${1:Name} {\n    $0\n}"),
            "object" => Some("object ${1:Name} {\n    $0\n}"),
            "enum" => Some("enum class ${1:Name} {\n    ${0:VALUE}\n}"),
            "if" => Some("if (${1:condition}) {\n    $0\n}"),
            "ife" => Some("if (${1:condition}) {\n    $2\n} else {\n    $0\n}"),
            "for" => Some("for (${1:item} in ${2:items}) {\n    $0\n}"),
            "while" => Some("while (${1:condition}) {\n    $0\n}"),
            "when" => Some("when (${1:value}) {\n    ${2:pattern} -> ${3:TODO()}\n    else -> ${0:TODO()}\n}"),
            "try" => Some("try {\n    $1\n} catch (${2:ex}: ${3:Exception}) {\n    $0\n}"),
            "val" => Some("val ${1:name} = ${2:value}$0"),
            "var" => Some("var ${1:name} = ${2:value}$0"),
            "println" => Some("println($0)"),
            _ => None,
        },
        "Swift" => {
            "fn" | "func" => Some("func ${1:name}($2) -> ${3:Void} {\n    $0\n}"),
            "class" => Some("class ${1:Name} {\n    $0\n}"),
            "struct" => Some("struct ${1:Name} {\n    $0\n}"),
            "enum" => Some("enum ${1:Name} {\n    case ${0:value}\n}"),
            "protocol" => Some("protocol ${1:Name} {\n    $0\n}"),
            "extension" => Some("extension ${1:Type} {\n    $0\n}"),
            "init" => Some("init($1) {\n    $0\n}"),
            "if" => Some("if ${1:condition} {\n    $0\n}"),
            "ife" => Some("if ${1:condition} {\n    $2\n} else {\n    $0\n}"),
            "iflet" => Some("if let ${1:value} = ${2:optional} {\n    $0\n}"),
            "guard" => Some("guard ${1:condition} else {\n    ${0:return}\n}"),
            "for" => Some("for ${1:item} in ${2:items} {\n    $0\n}"),
            "switch" => Some("switch ${1:value} {\ncase ${2:pattern}:\n    $0\ndefault:\n    break\n}"),
            "do" => Some("do {\n    $1\n} catch {\n    $0\n}"),
            "let" => Some("let ${1:name} = ${2:value}$0"),
            "print" => Some("print($0)"),
            _ => None,
        },
        "Dart" => {
            "main" => Some("void main() {\n  $0\n}"),
            "fn" | "func" => Some("${1:void} ${2:name}($3) {\n  $0\n}"),
            "afn" => Some("Future<${1:void}> ${2:name}($3) async {\n  $0\n}"),
            "class" => Some("class ${1:Name} {\n  $0\n}"),
            "enum" => Some("enum ${1:Name} { ${0:value} }"),
            "extension" => Some("extension ${1:Name} on ${2:Type} {\n  $0\n}"),
            "if" => Some("if (${1:condition}) {\n  $0\n}"),
            "ife" => Some("if (${1:condition}) {\n  $2\n} else {\n  $0\n}"),
            "for" => Some("for (var i = 0; i < ${1:count}; i++) {\n  $0\n}"),
            "fore" => Some("for (final ${1:item} in ${2:items}) {\n  $0\n}"),
            "while" => Some("while (${1:condition}) {\n  $0\n}"),
            "try" => Some("try {\n  $1\n} catch (${2:error}) {\n  $0\n}"),
            "final" => Some("final ${1:name} = ${2:value};$0"),
            "const" => Some("const ${1:name} = ${2:value};$0"),
            "imp" => Some("import '${1:library}';\n$0"),
            "print" => Some("print($0);"),
            _ => None,
        },
        "Scala" => {
            "def" | "fn" => Some("def ${1:name}($2): ${3:Unit} = {\n  $0\n}"),
            "class" => Some("class ${1:Name} {\n  $0\n}"),
            "caseclass" => Some("case class ${1:Name}(${2:value: String})$0"),
            "object" => Some("object ${1:Name} {\n  $0\n}"),
            "trait" => Some("trait ${1:Name} {\n  $0\n}"),
            "if" => Some("if (${1:condition}) {\n  $0\n}"),
            "ife" => Some("if (${1:condition}) {\n  $2\n} else {\n  $0\n}"),
            "for" => Some("for (${1:item} <- ${2:items}) {\n  $0\n}"),
            "fory" => Some("for {\n  ${1:item} <- ${2:items}\n} yield ${0:item}"),
            "match" => Some("${1:value} match {\n  case ${2:pattern} => ${0:()}\n}"),
            "try" => Some("try {\n  $1\n} catch {\n  case ${2:ex}: ${3:Exception} => ${0:()}\n}"),
            "val" => Some("val ${1:name} = ${2:value}$0"),
            "imp" => Some("import ${1:package}\n$0"),
            "println" => Some("println($0)"),
            _ => None,
        },
        "R" => {
            "fn" | "function" => Some("${1:name} <- function($2) {\n  $0\n}"),
            "if" => Some("if (${1:condition}) {\n  $0\n}"),
            "ife" => Some("if (${1:condition}) {\n  $2\n} else {\n  $0\n}"),
            "for" => Some("for (${1:item} in ${2:items}) {\n  $0\n}"),
            "while" => Some("while (${1:condition}) {\n  $0\n}"),
            "repeat" => Some("repeat {\n  $0\n}"),
            "library" => Some("library(${1:package})\n$0"),
            "source" => Some("source(\"${1:file.R}\")\n$0"),
            "readcsv" => Some("${1:data} <- read.csv(\"${2:file.csv}\")$0"),
            "writecsv" => Some("write.csv(${1:data}, \"${2:file.csv}\", row.names = FALSE)$0"),
            "df" => Some("data.frame(${1:name} = ${2:values})$0"),
            "col" => Some("${1:data}\\$${2:column}$0"),
            "plot" => Some("plot(${1:x}, ${2:y})$0"),
            "print" => Some("print($0)"),
            "try" => Some("tryCatch({\n  $1\n}, error = function(${2:error}) {\n  $0\n})"),
            _ => None,
        },
        "Elixir" => {
            "defmodule" => Some("defmodule ${1:Name} do\n  $0\nend"),
            "def" => Some("def ${1:name}($2) do\n  $0\nend"),
            "defp" => Some("defp ${1:name}($2) do\n  $0\nend"),
            "if" => Some("if ${1:condition} do\n  $0\nend"),
            "ife" => Some("if ${1:condition} do\n  $2\nelse\n  $0\nend"),
            "case" => Some("case ${1:value} do\n  ${2:pattern} -> ${0:nil}\nend"),
            "cond" => Some("cond do\n  ${1:condition} -> ${2:nil}\n  true -> ${0:nil}\nend"),
            "for" => Some("for ${1:item} <- ${2:items} do\n  $0\nend"),
            "with" => Some("with ${1:pattern} <- ${2:expression} do\n  $0\nend"),
            "try" => Some("try do\n  $1\nrescue\n  ${2:error} -> ${0:nil}\nend"),
            "inspect" => Some("IO.inspect(${1:value})$0"),
            "test" => Some("test \"${1:description}\" do\n  $0\nend"),
            _ => None,
        },
        "Haskell" => {
            "module" => Some("module ${1:Name} where\n\n$0"),
            "imp" | "import" => Some("import ${1:Module}\n$0"),
            "importq" => Some("import qualified ${1:Module} as ${2:Alias}\n$0"),
            "fn" => Some("${1:name} ${2:args} = ${0:undefined}"),
            "sig" => Some("${1:name} :: ${2:Type}$0"),
            "data" => Some("data ${1:Name} = ${2:Constructor}\n  deriving (${3:Show, Eq})\n$0"),
            "newtype" => Some("newtype ${1:Name} = ${2:Constructor} ${3:Type}\n$0"),
            "type" => Some("type ${1:Name} = ${2:Type}\n$0"),
            "case" => Some("case ${1:value} of\n  ${2:pattern} -> ${0:undefined}"),
            "if" => Some("if ${1:condition}\n  then ${2:undefined}\n  else ${0:undefined}"),
            "let" => Some("let ${1:name} = ${2:value}\nin ${0:undefined}"),
            "do" => Some("do\n  ${0:pure ()}"),
            _ => None,
        },
        "Erlang" => {
            "module" => Some("-module(${1:name}).\n$0"),
            "export" => Some("-export([${1:name}/${2:arity}]).\n$0"),
            "fn" | "fun" => Some("${1:name}($2) ->\n    ${0:ok}."),
            "case" => Some("case ${1:Value} of\n    ${2:pattern} -> ${0:ok}\nend"),
            "receive" => Some("receive\n    ${1:Message} -> ${0:ok}\nend"),
            "if" => Some("if\n    ${1:true} -> ${0:ok}\nend"),
            "try" => Some("try\n    ${1:expression}\ncatch\n    ${2:Class}:${3:Reason} -> ${0:ok}\nend"),
            "record" => Some("-record(${1:name}, {${2:field}}).\n$0"),
            "define" => Some("-define(${1:NAME}, ${2:value}).\n$0"),
            "spec" => Some("-spec ${1:name}($2) -> ${3:term()}.$0"),
            _ => None,
        },
        "Clojure" | "ClojureScript" => {
            "ns" => Some("(ns ${1:name})\n\n$0"),
            "def" => Some("(def ${1:name} ${2:value})$0"),
            "defn" => Some("(defn ${1:name} [$2]\n  $0)"),
            "let" => Some("(let [${1:name} ${2:value}]\n  $0)"),
            "if" => Some("(if ${1:condition}\n  ${2:nil}\n  ${0:nil})"),
            "when" => Some("(when ${1:condition}\n  $0)"),
            "iflet" => Some("(if-let [${1:name} ${2:expression}]\n  ${3:nil}\n  ${0:nil})"),
            "doseq" => Some("(doseq [${1:item} ${2:items}]\n  $0)"),
            "for" => Some("(for [${1:item} ${2:items}]\n  ${0:nil})"),
            "println" => Some("(println ${1:value})$0"),
            _ => None,
        },
        "SQL" | "SQL (Basic)" | "PostgreSQL" | "MySQL" | "SQLite" | "T-SQL" | "TSQL" | "SQL (Rails)" => {
            "select" | "sel" => Some("SELECT ${1:columns}\nFROM ${2:table_name}\nWHERE ${3:condition};$0"),
            "insert" | "ins" => Some("INSERT INTO ${1:table_name} (${2:columns})\nVALUES (${3:values});$0"),
            "update" | "upd" => Some("UPDATE ${1:table_name}\nSET ${2:column_name} = ${3:value}\nWHERE ${4:condition};$0"),
            "delete" | "del" => Some("DELETE FROM ${1:table_name}\nWHERE ${2:condition};$0"),
            "join" => Some("JOIN ${1:table_name}\n    ON ${2:condition}$0"),
            "leftjoin" => Some("LEFT JOIN ${1:table_name}\n    ON ${2:condition}$0"),
            "cte" | "with" => Some("WITH ${1:name} AS (\n    ${2:SELECT 1}\n)\n${0:SELECT 1};"),
            "create" => Some("CREATE TABLE ${1:table_name} (\n    ${0:id INTEGER PRIMARY KEY}\n);"),
            "view" => Some("CREATE VIEW ${1:view_name} AS\n${0:SELECT 1};"),
            "index" => Some("CREATE INDEX ${1:index_name}\nON ${2:table_name} (${3:column_name});$0"),
            "alter" => Some("ALTER TABLE ${1:table_name}\nADD ${2:column_name} ${3:TEXT};$0"),
            "group" => Some("GROUP BY ${1:columns}$0"),
            "order" => Some("ORDER BY ${1:column_name} ${2:ASC}$0"),
            _ => None,
        },
        "Markdown" | "MultiMarkdown" => {
            "h1" => Some("# ${1:Heading}\n\n$0"),
            "h2" => Some("## ${1:Heading}\n\n$0"),
            "h3" => Some("### ${1:Heading}\n\n$0"),
            "code" => Some("```${1:language}\n$0\n```"),
            "link" => Some("[${1:text}](${2:url})$0"),
            "image" | "img" => Some("![${1:alt}](${2:path})$0"),
            "table" => Some("| ${1:Column 1} | ${2:Column 2} |\n| --- | --- |\n| ${3:Value 1} | ${4:Value 2} |\n$0"),
            "task" => Some("- [ ] ${1:Task}$0"),
            "quote" => Some("> ${1:Quote}\n\n$0"),
            "list" => Some("- ${1:Item}\n- $0"),
            "olist" => Some("1. ${1:Item}\n2. $0"),
            "details" => Some("<details>\n<summary>${1:Summary}</summary>\n\n$0\n\n</details>"),
            _ => None,
        },
        "JSON" | "JSONC" | "JSON5" | "JSON with Comments" | "JSON (Go)" | "JSON (PHP)" | "JSON (Rails)" | "JSON (Terraform)" => {
            "obj" | "object" => Some("{\n    \"${1:key}\": ${2:null}\n}$0"),
            "arr" | "array" => Some("[\n    $0\n]"),
            "str" => Some("\"${1:key}\": \"${2:value}\"$0"),
            "num" => Some("\"${1:key}\": ${2:0}$0"),
            "bool" => Some("\"${1:key}\": ${2:true}$0"),
            "null" => Some("\"${1:key}\": null$0"),
            "prop" => Some("\"${1:key}\": ${2:null}$0"),
            _ => None,
        },
        "YAML" | "YAML (Go)" => {
            "pair" => Some("${1:key}: ${2:value}$0"),
            "map" => Some("${1:key}:\n  ${2:child}: ${3:value}\n$0"),
            "list" => Some("${1:key}:\n  - ${2:item}\n  - $0"),
            "obj" => Some("${1:key}:\n  - ${2:name}: ${3:value}\n    $0"),
            "block" => Some("${1:key}: |\n  $0"),
            "fold" => Some("${1:key}: >\n  $0"),
            "anchor" => Some("${1:key}: &${2:anchor}\n  $0"),
            "alias" => Some("${1:key}: *${2:anchor}$0"),
            "doc" => Some("---\n$0"),
            _ => None,
        },
        "TOML" | "TOML (Cargo)" => {
            "table" => Some("[${1:table}]\n$0"),
            "arraytable" => Some("[[${1:table}]]\n$0"),
            "str" => Some("${1:key} = \"${2:value}\"$0"),
            "num" => Some("${1:key} = ${2:0}$0"),
            "bool" => Some("${1:key} = ${2:true}$0"),
            "array" => Some("${1:key} = [${2:1, 2, 3}]$0"),
            "inline" => Some("${1:key} = { ${2:name} = ${3:\"value\"} }$0"),
            "multiline" => Some("${1:key} = \"\"\"\n$0\n\"\"\""),
            _ => None,
        },
        "CMake" => {
            "project" => Some("cmake_minimum_required(VERSION ${1:3.16})\nproject(${2:ProjectName} LANGUAGES ${3:CXX})\n\n$0"),
            "exe" | "add_executable" => Some("add_executable(${1:target} ${2:sources})\n$0"),
            "lib" | "add_library" => Some("add_library(${1:target} ${2:STATIC} ${3:sources})\n$0"),
            "link" | "target_link_libraries" => Some("target_link_libraries(${1:target} PRIVATE ${2:library})\n$0"),
            "inc" | "target_include_directories" => Some("target_include_directories(${1:target} PRIVATE \\${CMAKE_CURRENT_SOURCE_DIR}/${2:include})\n$0"),
            "set" => Some("set(${1:NAME} ${2:value})\n$0"),
            "option" => Some("option(${1:NAME} \"${2:Description}\" ${3:OFF})\n$0"),
            "if" => Some("if(${1:condition})\n    $0\nendif()"),
            "foreach" => Some("foreach(${1:item} IN LISTS ${2:items})\n    $0\nendforeach()"),
            "fn" | "function" => Some("function(${1:name} $2)\n    $0\nendfunction()"),
            "find" => Some("find_package(${1:Package} REQUIRED)\n$0"),
            "subdir" => Some("add_subdirectory(${1:directory})\n$0"),
            "message" => Some("message(STATUS \"${1:message}\")$0"),
            "test" => Some("add_test(NAME ${1:name} COMMAND ${2:command})\n$0"),
            _ => None,
        },
        "Makefile" => {
            "target" => Some("${1:target}: ${2:dependencies}\n\t${0:command}"),
            "phony" => Some(".PHONY: ${1:target}\n$0"),
            "var" => Some("${1:NAME} := ${2:value}$0"),
            "default" => Some("${1:NAME} ?= ${2:value}$0"),
            "append" => Some("${1:NAME} += ${2:value}$0"),
            "rule" => Some("%.o: %.c\n\t\\$(CC) \\$(CPPFLAGS) \\$(CFLAGS) -c \\$< -o \\$@\n$0"),
            "ifeq" => Some("ifeq (\\$(${1:VARIABLE}),${2:value})\n$0\nendif"),
            "define" => Some("define ${1:NAME}\n$0\nendef"),
            "include" => Some("include ${1:file.mk}\n$0"),
            "call" => Some("\\$(call ${1:name},${2:argument})$0"),
            _ => None,
        },
        "Dockerfile" | "Dockerfile (with bash)" => {
            "from" => Some("FROM ${1:base:tag}\n$0"),
            "run" => Some("RUN ${0:command}"),
            "copy" => Some("COPY ${1:source} ${2:destination}$0"),
            "add" => Some("ADD ${1:source} ${2:destination}$0"),
            "workdir" => Some("WORKDIR ${1:/app}\n$0"),
            "env" => Some("ENV ${1:NAME}=${2:value}\n$0"),
            "arg" => Some("ARG ${1:NAME}=${2:default}\n$0"),
            "expose" => Some("EXPOSE ${1:8080}\n$0"),
            "cmd" => Some("CMD [\"${1:executable}\"$2]$0"),
            "entrypoint" => Some("ENTRYPOINT [\"${1:executable}\"$2]$0"),
            "user" => Some("USER ${1:app}\n$0"),
            "volume" => Some("VOLUME [\"${1:/data}\"]$0"),
            "label" => Some("LABEL ${1:key}=\"${2:value}\"$0"),
            "healthcheck" => Some("HEALTHCHECK CMD ${0:command}"),
            _ => None,
        },
        "Terraform" | "Terraform (variables)" | "HCL" => {
            "resource" => Some("resource \"${1:type}\" \"${2:name}\" {\n  $0\n}"),
            "data" => Some("data \"${1:type}\" \"${2:name}\" {\n  $0\n}"),
            "variable" => Some("variable \"${1:name}\" {\n  type        = ${2:string}\n  description = \"${3:Description}\"\n  $0\n}"),
            "output" => Some("output \"${1:name}\" {\n  value = ${2:expression}\n  $0\n}"),
            "locals" => Some("locals {\n  ${1:name} = ${2:value}\n  $0\n}"),
            "module" => Some("module \"${1:name}\" {\n  source = \"${2:./module}\"\n  $0\n}"),
            "provider" => Some("provider \"${1:name}\" {\n  $0\n}"),
            "terraform" => Some("terraform {\n  $0\n}"),
            _ => None,
        },
        "GraphQL" => {
            "query" => Some("query ${1:Name} {\n  ${0:field}\n}"),
            "mutation" => Some("mutation ${1:Name} {\n  ${0:field}\n}"),
            "subscription" => Some("subscription ${1:Name} {\n  ${0:field}\n}"),
            "fragment" => Some("fragment ${1:Name} on ${2:Type} {\n  ${0:field}\n}"),
            "type" => Some("type ${1:Name} {\n  ${0:id: ID!}\n}"),
            "input" => Some("input ${1:Name} {\n  ${0:value: String}\n}"),
            "enum" => Some("enum ${1:Name} {\n  ${0:VALUE}\n}"),
            "interface" => Some("interface ${1:Name} {\n  ${0:id: ID!}\n}"),
            "scalar" => Some("scalar ${1:Name}$0"),
            _ => None,
        },
        "Protocol Buffers" | "Protocol Buffer" | "Protobuf" => {
            "syntax" => Some("syntax = \"proto3\";\n\n$0"),
            "package" => Some("package ${1:name};\n\n$0"),
            "import" => Some("import \"${1:file.proto}\";\n$0"),
            "message" => Some("message ${1:Name} {\n  $0\n}"),
            "enum" => Some("enum ${1:Name} {\n  ${2:VALUE_UNSPECIFIED} = 0;\n  $0\n}"),
            "field" => Some("${1:string} ${2:name} = ${3:1};$0"),
            "repeated" => Some("repeated ${1:string} ${2:items} = ${3:1};$0"),
            "map" => Some("map<${1:string}, ${2:string}> ${3:items} = ${4:1};$0"),
            "oneof" => Some("oneof ${1:name} {\n  ${0:string value = 1;}\n}"),
            "service" => Some("service ${1:Name} {\n  $0\n}"),
            "rpc" => Some("rpc ${1:Name} (${2:Request}) returns (${3:Response});$0"),
            _ => None,
        },
        _ => None,
}

/// Parse a snippet body and return (final_text, vec_of_(index, char_offset, placeholder_len))
pub(crate) fn parse_snippet_body(body: &str) -> (String, Vec<(u32, usize, usize)>) {
    let mut result = String::with_capacity(body.len());
    let mut tab_stops: Vec<(u32, usize, usize)> = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '$' && i + 1 < len {
            if chars[i + 1] == '{' {
                // ${N:placeholder} or ${N|choice1,choice2|}
                i += 2;
                let num_start = i;
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let index: u32 = chars[num_start..i].iter()
                    .fold(0u32, |acc, &c| acc * 10 + c.to_digit(10).unwrap_or(0));
                if i < len && chars[i] == ':' {
                    // ${N:placeholder}
                    i += 1;
                    let placeholder_start = result.chars().count();
                    let mut depth = 1;
                    while i < len {
                        if chars[i] == '}' {
                            depth -= 1;
                            if depth == 0 { i += 1; break; }
                        } else if chars[i] == '{' {
                            depth += 1;
                        } else if chars[i] == '\\' && i + 1 < len {
                            result.push(chars[i + 1]);
                            i += 2;
                            continue;
                        }
                        result.push(chars[i]);
                        i += 1;
                    }
                    let placeholder_len = result.chars().count() - placeholder_start;
                    tab_stops.push((index, placeholder_start, placeholder_len));
                } else if i < len && chars[i] == '|' {
                    // ${N|choice1,choice2|} - use first choice as default
                    i += 1;
                    let placeholder_start = result.chars().count();
                    let mut first_choice = String::new();
                    while i < len && chars[i] != ',' && chars[i] != '|' {
                        first_choice.push(chars[i]);
                        i += 1;
                    }
                    // Skip rest of choices
                    while i < len && chars[i] != '}' {
                        i += 1;
                    }
                    if i < len { i += 1; } // skip '}'
                    result.push_str(&first_choice);
                    let placeholder_len = first_choice.chars().count();
                    tab_stops.push((index, placeholder_start, placeholder_len));
                } else {
                    // ${N} - no placeholder
                    if i < len && chars[i] == '}' { i += 1; }
                    tab_stops.push((index, result.chars().count(), 0));
                }
            } else if chars[i + 1].is_ascii_digit() {
                // $N
                i += 1;
                let num_start = i;
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let index: u32 = chars[num_start..i].iter()
                    .fold(0u32, |acc, &c| acc * 10 + c.to_digit(10).unwrap_or(0));
                tab_stops.push((index, result.chars().count(), 0));
            } else {
                result.push(chars[i]);
                i += 1;
            }
        } else if chars[i] == '\\' && i + 1 < len && matches!(chars[i + 1], '$' | '}' | '\\') {
            result.push(chars[i + 1]);
            i += 2;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    (result, tab_stops)
}

impl Editor {
    /// Accept a popup snippet as one edit, including replacement of a partial
    /// trigger and its suffix. Undo restores exactly the user's original text.
    pub(crate) fn complete_snippet(&mut self, trigger: &str) -> bool {
        crate::editor_document::edit!(self, {
            let Some(body) = get_snippet(trigger, self.language_name()) else {
                return false;
            };
            let line = self.rope.line(self.cursor_line);
            let cursor = self.cursor_col.min(line.len_chars());
            let word = |ch: &char| ch.is_alphanumeric() || matches!(ch, '_' | '$');
            let before = line
                .chars_at(cursor)
                .reversed()
                .take(4097)
                .take_while(word)
                .count();
            let after = line.chars_at(cursor).take(4097).take_while(word).count();
            if before > 4096 || after > 4096 {
                return false;
            }
            let indent: String = line
                .chars()
                .take(4097)
                .take_while(|ch| matches!(ch, ' ' | '\t'))
                .collect();
            if indent.len() > 4096 {
                return false;
            }
            let expanded = body.replace('\n', &format!("\n{indent}"));
            let start = self.rope.line_to_char(self.cursor_line) + cursor - before;
            let end = self.rope.line_to_char(self.cursor_line) + cursor + after;
            self.save_undo();
            self.rope.remove(start..end);
            self.insert_snippet_text(start, &expanded);
            true
        })
    }

    /// Try to expand a snippet trigger word before the cursor
    pub(crate) fn try_expand_snippet(&mut self) -> bool {
        crate::editor_document::edit!(self, {
        let line = self.rope.line(self.cursor_line);
        let end = self.cursor_col.min(line.len_chars());
        let mut before = line.chars_at(end).reversed().peekable();
        if !before.peek().is_some_and(|&c| c.is_alphanumeric() || c == '_') { return false; }
        let lang = self.language_name();
        let max_bytes = max_snippet_trigger_bytes(lang);
        if max_bytes == 0 { return false; }
        // Tab usually has no snippet to expand. Seek directly to its local
        // trigger instead of copying the logical line and its entire prefix.
        let mut trigger_chars = 0;
        let mut trigger_bytes = 0;
        for c in before.take_while(|&c| c.is_alphanumeric() || c == '_') {
            trigger_bytes += c.len_utf8();
            if trigger_bytes > max_bytes { return false; }
            trigger_chars += 1;
        }
        if trigger_chars == 0 { return false; }
        let word_start_chars = end - trigger_chars;
        let trigger = line.slice(word_start_chars..end);
        let trigger: std::borrow::Cow<'_, str> = trigger.as_str()
            .map(std::borrow::Cow::Borrowed)
            .unwrap_or_else(|| std::borrow::Cow::Owned(trigger.to_string()));

        let snippet = get_snippet(&trigger, lang);

        if let Some(body) = snippet {
            let expanded = if body.contains('\n') {
                let indent: String = line.chars()
                    .take_while(|&c| c.is_whitespace() && c != '\n' && c != '\r').collect();
                std::borrow::Cow::Owned(body.replace('\n', &format!("\n{indent}")))
            } else {
                std::borrow::Cow::Borrowed(body)
            };
            self.save_undo();
            let line_start = self.rope.line_to_char(self.cursor_line);
            let trigger_start = line_start + word_start_chars;
            let trigger_end = line_start + self.cursor_col;
            self.rope.remove(trigger_start..trigger_end);

            self.insert_snippet_text(trigger_start, &expanded);
            return true;
        }

        false
            })
    }

    /// Insert snippet text with tab stop parsing at the given char position.
    /// Handles $0, $1, $2, ${1:placeholder} syntax.
    pub(crate) fn insert_snippet_text(&mut self, insert_pos: usize, snippet_body: &str) {
        crate::editor_document::edit!(self, {
        let (text, stops) = parse_snippet_body(snippet_body);
        self.insert_parsed_snippet_text(insert_pos, text, stops);
            })
    }

    pub(crate) fn insert_parsed_snippet_text(&mut self, insert_pos: usize, final_text: String,
        tab_stops: Vec<(u32, usize, usize)>)
    {
        crate::editor_document::edit!(self, {
        self.snippet_cancel();
        let changed_from = self.rope.char_to_line(insert_pos.min(self.rope.len_chars()));
        let final_chars = final_text.chars().count();

        self.rope.insert(insert_pos, &final_text);

        // Convert char offsets to (line, col) positions
        self.snippet_tab_stops.clear();
        for (index, offset, placeholder_len) in &tab_stops {
            let abs_pos = insert_pos + *offset;
            let line = self.rope.char_to_line(abs_pos.min(self.rope.len_chars()));
            let col = abs_pos - self.rope.line_to_char(line);
            self.snippet_tab_stops.push(SnippetTabStop {
                index: *index,
                line,
                col,
                placeholder_len: *placeholder_len,
            });
        }

        // Sort tab stops by index (put $0 last)
        self.snippet_tab_stops.sort_by_key(|s| if s.index == 0 { u32::MAX } else { s.index });

        if !self.snippet_tab_stops.is_empty() {
            self.snippet_active = true;
            self.snippet_active_stop = 0;
            self.snippet_rope = Some(self.rope.clone());
            self.select_snippet_stop();
        } else {
            // No tab stops - just position at end
            let end_pos = insert_pos + final_chars;
            let line = self.rope.char_to_line(end_pos.min(self.rope.len_chars()));
            let col = end_pos - self.rope.line_to_char(line);
            self.cursor_line = line;
            self.cursor_col = col;
            self.clear_selection();
        }

        self.modified = true;
        self.desired_col = None;
        self.mark_syntax_dirty_range(changed_from, self.rope.char_to_line(insert_pos + final_chars));
        self.content_version += 1;
        self.ensure_visible();
            })
    }

    /// Advance to the next snippet tab stop. Returns true if handled.
    pub(crate) fn snippet_next_tab_stop(&mut self) -> bool {
        if !self.refresh_snippet_stops() {
            return false;
        }
        self.break_undo_group();
        self.snippet_active_stop += 1;
        if self.snippet_active_stop >= self.snippet_tab_stops.len() {
            // All stops visited - exit snippet mode
            self.snippet_cancel();
            self.clear_selection();
            return true;
        }
        self.select_snippet_stop();
        self.ensure_visible();
        true
    }

    /// Go back to previous snippet tab stop. Returns true if handled.
    pub(crate) fn snippet_prev_tab_stop(&mut self) -> bool {
        if !self.refresh_snippet_stops() || self.snippet_active_stop == 0 {
            return false;
        }
        self.break_undo_group();
        self.snippet_active_stop -= 1;
        self.select_snippet_stop();
        self.ensure_visible();
        true
    }

    fn select_snippet_stop(&mut self) {
        let stop = &self.snippet_tab_stops[self.snippet_active_stop];
        let end = self.rope.line_to_char(stop.line) + stop.col + stop.placeholder_len;
        self.selection_anchor = (stop.placeholder_len > 0).then_some((stop.line, stop.col));
        self.cursor_line = self.rope.char_to_line(end);
        self.cursor_col = end - self.rope.line_to_char(self.cursor_line);
        self.desired_col = None;
    }

    fn refresh_snippet_stops(&mut self) -> bool {
        if !self.snippet_active { return false; }
        let Some(previous) = self.snippet_rope.take() else {
            self.snippet_cancel();
            return false;
        };
        if previous.is_instance(&self.rope) {
            self.snippet_rope = Some(previous);
            return true;
        }
        let mut ranges: Vec<_> = self.snippet_tab_stops.iter().map(|stop| {
            let start = previous.line_to_char(stop.line) + stop.col;
            (start, start + stop.placeholder_len)
        }).collect();
        let Some(&(active_start, active_end)) = ranges.get(self.snippet_active_stop) else {
            self.snippet_cancel();
            return false;
        };
        // Anchor the comparison to this placeholder: a minimal diff can place
        // an inserted repeated character in the identical text just outside it.
        // Only Tab navigation compares shared chunks; typing adds no scan.
        let Some(new_end) = self.rope.len_chars().checked_sub(previous.len_chars() - active_end)
            .filter(|end| *end >= active_start) else {
            self.snippet_cancel();
            return false;
        };
        if !crate::rope_changes::equal_slices(previous.slice(..active_start), self.rope.slice(..active_start))
            || !crate::rope_changes::equal_slices(previous.slice(active_end..), self.rope.slice(new_end..))
        {
            self.snippet_cancel();
            return false;
        }
        let removed = active_end - active_start;
        let inserted = new_end - active_start;
        for (index, (start, end)) in ranges.iter_mut().enumerate() {
            if index == self.snippet_active_stop {
                *end = *end - removed + inserted;
            } else if *start >= active_end
                && (*start > active_end || active_end > active_start || index > self.snippet_active_stop)
            {
                *start = *start - removed + inserted;
                *end = *end - removed + inserted;
            } else if *end > active_start {
                self.snippet_cancel();
                return false;
            }
        }
        for (stop, (start, end)) in self.snippet_tab_stops.iter_mut().zip(ranges) {
            stop.line = self.rope.char_to_line(start);
            stop.col = start - self.rope.line_to_char(stop.line);
            stop.placeholder_len = end - start;
        }
        self.snippet_rope = Some(self.rope.clone());
        true
    }

    /// Cancel snippet mode (e.g. on Escape)
    pub(crate) fn snippet_cancel(&mut self) {
        self.snippet_active = false;
        self.snippet_tab_stops.clear();
        self.snippet_active_stop = 0;
        self.snippet_rope = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_snippet_replaces_a_partial_word_and_suffix_as_one_undoable_edit() {
        let mut editor = Editor::test_with_text("    fnoisy");
        editor.file_path = Some(std::path::PathBuf::from("test.rs"));
        editor.cursor_col = 5;
        let original = editor.rope.to_string();
        assert!(editor.complete_snippet("fn"));
        assert!(editor.rope.to_string().starts_with("    fn "));
        assert!(!editor.rope.to_string().contains("noisy"));
        assert!(editor.snippet_active);
        assert!(!editor.snippet_tab_stops.is_empty());
        assert!(
            editor
                .rope
                .to_string()
                .lines()
                .skip(1)
                .all(|line| line.is_empty() || line.starts_with("    "))
        );
        editor.undo();
        assert_eq!(editor.rope.to_string(), original);
    }

    #[test]
    fn test_parse_simple_tabstop() {
        let (text, stops) = parse_snippet_body("hello $1 world $0");
        assert_eq!(text, "hello  world ");
        assert_eq!(stops.len(), 2);
        // $1 at char offset 6 (after "hello ")
        assert_eq!(stops[0].0, 1);
        assert_eq!(stops[0].1, 6);
        assert_eq!(stops[0].2, 0);
        // $0 at char offset 13 (after "hello  world ")
        assert_eq!(stops[1].0, 0);
        assert_eq!(stops[1].1, 13);
        assert_eq!(stops[1].2, 0);
    }

    #[test]
    fn test_parse_placeholder_tabstop() {
        let (text, stops) = parse_snippet_body("${1:default_value}");
        assert_eq!(text, "default_value");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].0, 1);
        assert_eq!(stops[0].1, 0);
        assert_eq!(stops[0].2, 13); // "default_value".len()
    }

    #[test]
    fn test_parse_choice_tabstop() {
        let (text, stops) = parse_snippet_body("${1|opt1,opt2,opt3|}");
        assert_eq!(text, "opt1");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].0, 1);
        assert_eq!(stops[0].1, 0);
        assert_eq!(stops[0].2, 4); // "opt1".len()
    }

    #[test]
    fn test_get_snippet_rust_fn() {
        let snippet = get_snippet("fn", "Rust");
        assert!(snippet.is_some());
        let body = snippet.unwrap();
        assert!(body.contains("fn "));
        assert!(body.contains("$0"));
    }

    #[test]
    fn test_get_snippet_unknown_trigger() {
        let snippet = get_snippet("nonexistent_trigger_xyz", "Rust");
        assert!(snippet.is_none());
    }

    #[test]
    fn test_get_snippet_javascript_for() {
        let snippet = get_snippet("for", "JavaScript");
        assert!(snippet.is_some());
        let body = snippet.unwrap();
        assert!(body.contains("for"));
    }

    #[test]
    fn test_snippet_body_with_newlines() {
        let (text, stops) = parse_snippet_body("line1\nline2\n$0");
        assert_eq!(text, "line1\nline2\n");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].0, 0);
    }
}
