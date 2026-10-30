function __vtcode_preexec --on-event fish_preexec
    printf '\e]633;C\a'
end
function __vtcode_postexec --on-event fish_postexec
    set -l result $status
    printf '\e]633;D;%s\a' $result
end
