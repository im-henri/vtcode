# Preserve the normal interactive profile, including its prompt and hooks.
if [[ -f ~/.bashrc ]]; then . ~/.bashrc; fi

__vtcode_precmd() {
    local result=$?
    # Bash before 4.4 has no PS0. History changes provide a completion fallback.
    if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
        if [[ -n ${__vtcode_history_ready-} && ${__vtcode_history-} != "${HISTCMD-}" ]]; then
            printf '\033]633;C\007'
        fi
        __vtcode_history=${HISTCMD-}
        __vtcode_history_ready=1
    fi
    printf '\033]633;D;%s\007' "$result"
    # Existing prompt hooks must still receive the command's exit status.
    return "$result"
}

if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    PS0=$'\033]633;C\007'${PS0-}
fi
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(__vtcode_precmd "${PROMPT_COMMAND[@]}")
else
    PROMPT_COMMAND="__vtcode_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
