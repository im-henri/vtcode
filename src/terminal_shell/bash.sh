# Preserve the normal interactive profile, including its prompt and hooks.
if [[ -f ~/.bashrc ]]; then . ~/.bashrc; fi

__vtcode_precmd() {
    local result=$?
    # Bash before 4.4 has no PS0. History changes provide a completion fallback.
    if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
        # Older Bash keeps HISTCMD fixed inside prompt hooks. Read the current
        # history entry without timestamps so empty prompts remain unchanged.
        local history
        history=$(HISTTIMEFORMAT= builtin history 1)
        if [[ -n ${__vtcode_history_ready-} && ${__vtcode_history-} != "$history" ]]; then
            printf '\033]633;C\007'
        fi
        __vtcode_history=$history
        __vtcode_history_ready=1
    fi
    printf '\033]633;D;%s\007' "$result"
    # Existing prompt hooks must still receive the command's exit status.
    return "$result"
}

if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    PS0=$'\033]633;C\007'${PS0-}
fi
# Prefix element zero in place, preserving scalar and array prompt hooks.
# Older Bash executes only that element; newer Bash also runs later entries.
PROMPT_COMMAND="__vtcode_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
