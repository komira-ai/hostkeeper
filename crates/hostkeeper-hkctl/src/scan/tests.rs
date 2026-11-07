use super::*;

/// Catches a construct of a bash newer than 3.2 the scan lets through: each line here holds one,
/// and is refused once, on its own line.
#[test]
fn every_construct_newer_than_bash_3_2_is_refused_on_its_line() {
    let newer = [
        "declare -A seen",
        "local -n ref=x",
        "typeset -g x=1",
        "declare -r -l lower",
        "readonly -u UP=x",
        "mapfile -t lines",
        "readarray lines",
        "coproc cat",
        "compopt -o nospace",
        "echo \"${name,,}\"",
        "echo ${name^}",
        "echo ${list[0]^^}",
        "echo ${name@Q}",
        "case $x in a) echo ;& b) ;; esac",
        "case $x in a) echo ;;& esac",
        "cmd |& other",
        "cmd &>> log",
        "exec {fd}>file",
        "echo $BASHPID",
        "echo $EPOCHSECONDS",
        "wait -n",
        "wait -f 12",
        "read -r -i default x",
        "read -N 4 x",
        "[[ -v name ]]",
        "test -R ref",
        "shopt -s globstar",
        "shopt -s nullglob lastpipe",
        "printf '%(%F)T'",
        "echo ${list[-1]}",
        "echo {1..10..2}",
        "local -",
    ];
    for (index, text) in newer.iter().enumerate() {
        let script = format!(
            "#!/bin/bash\n# {text}\necho {}\n{text}\n",
            crate::quoted(text)
        );
        let found = newer_than_bash_3_2(&script);
        assert_eq!(found.len(), 1, "{text}: {found:?}");
        assert_eq!(found[0].line, 4, "{text}");
        assert!(!found[0].construct.is_empty(), "{index}");
    }
}

/// Catches the scan refusing what bash 3.2 has: the constructs the runtime uses.
#[test]
fn what_bash_3_2_has_passes() {
    let fine = [
        "local -a list=()",
        "declare +x name",
        "printf -v n '%d' \"'$c\"",
        "out+=$c",
        "[[ $x =~ $re ]]",
        "echo ${#list[@]} ${!list[@]} ${name:1:2} ${name#*,} ${name%%=*} ${name//a/b}",
        "echo ${table:n >> 18 & 63:1} $((a << 16 | b))",
        "read -r -d '' -n 1 c",
        "case $c in a) ;; esac",
        "x=$(<file)",
        "cmd >/dev/null 2>&1",
        "echo {a,b}",
        "echo {1..3}",
        "shopt -s nocasematch",
        "[[ -n $x ]]",
        "echo $# ${#x}",
    ];
    for text in fine {
        assert_eq!(newer_than_bash_3_2(text), [], "{text}");
        assert_eq!(secret_hazards(text), [], "{text}");
    }
}

/// Catches an export or a here-document the hazard scan lets through, and one it finds inside a
/// help text.
#[test]
fn an_export_or_a_here_document_is_a_hazard() {
    for (text, hazard) in [
        ("export X", 0),
        ("declare -x X=1", 0),
        ("local -rx X=1", 0),
        ("cat <<EOF", 1),
        ("cat <<-'EOF'", 1),
        ("cat << \"EOF\"", 1),
        ("cat <<\\EOF", 1),
        ("read x <<<\"$y\"", 1),
    ] {
        assert_eq!(
            secret_hazards(&format!("x=1\n{text}\n")),
            [Finding {
                line: 2,
                construct: HAZARDS[hazard].1,
            }],
            "{text}"
        );
    }
    assert_eq!(secret_hazards("echo 'export <<EOF' # export\n"), []);
}

/// Catches the code of a script read wrongly: a single-quoted or `$'...'` text kept (a help text
/// would be scanned), a double-quoted text dropped (its expansions would not be), a comment kept,
/// a `#` that starts no comment dropped, an escaped quote ending a quote, and a line lost.
#[test]
fn the_code_of_a_script_is_read_without_quoted_text_and_comments() {
    let script = "a 'b ;& c' \"d ${e,,}\" # f |&\n\
                  g $'h \\' ;& i' j\n\
                  k \"l \\\" m\" n\\'o\n\
                  'multi\nline' p\n\
                  echo $# ${#x} a#b\n\
                  # whole\n\
                  x=1;# after\n\
                  $'esc\\\nnext' q\n\
                  \"dq\\\nnext\" r\\\n\
                  s 'single \\' t";
    assert_eq!(
        code(script),
        [
            "a '' \"d ${e,,}\" ",
            "g '' j",
            "k \"l \\\" m\" n\\'o",
            "''",
            " p",
            "echo $# ${#x} a#b",
            "",
            "x=1;",
            "''",
            " q",
            "\"dq\\",
            "next\" r\\",
            "s '' t",
        ]
    );
    // The scans read the code: a construct in a double-quoted text counts, one in a single-quoted
    // text or a comment does not.
    assert_eq!(newer_than_bash_3_2(script).len(), 1);
    assert_eq!(
        newer_than_bash_3_2(script)[0],
        Finding {
            line: 1,
            construct: NEWER[4].1
        }
    );
}

/// Catches a pattern that does not compile, which the compiled tables would leave out and so
/// turn its check off: every table compiles whole.
#[test]
fn every_pattern_of_the_scans_compiles() {
    assert_eq!(NEWER_RE.len(), NEWER.len());
    assert_eq!(NEWER_QUOTED_RE.len(), NEWER_QUOTED.len());
    assert_eq!(HAZARDS_RE.len(), HAZARDS.len());
}
