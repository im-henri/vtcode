function __vtcode_preexec --on-event fish_preexec
    printf '\e]633;C\a'
end
function __vtcode_postexec --on-event fish_postexec
    set -l result $status
    printf '\e]633;D;%s\a' $result
end
function __vtcode_cwd --on-event fish_prompt
    set -l result $status
    if not set -q __vtcode_directory; or test "$__vtcode_directory" != "$PWD"
        set -g __vtcode_directory $PWD
        set -l directory (string escape --style=url -- $PWD | string replace -a '%2F' '/' | string replace -a '%2f' '/')
        printf '\e]7;file://%s\a' $directory
    end
    return $result
end
