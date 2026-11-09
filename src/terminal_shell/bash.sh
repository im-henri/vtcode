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
    if [[ ${__vtcode_directory-} != "$PWD" ]]; then
        __vtcode_directory=$PWD
        local directory=${PWD//%/%25}
        directory=${directory// /%20}
        directory=${directory//\#/%23}
        directory=${directory//\?/%3F}
        directory=${directory//;/%3B}
        directory=${directory//$'\a'/%07}
        directory=${directory//$'\e'/%1B}
        directory=${directory//$'\n'/%0A}
        directory=${directory//$'\r'/%0D}
        printf '\033]7;file://%s\007' "$directory"
    fi
    # Existing prompt hooks must still receive the command's exit status.
    return "$result"
}

if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    PS0=$'\033]633;C\007'${PS0-}
fi
# Prefix element zero in place, preserving scalar and array prompt hooks.
# Older Bash executes only that element; newer Bash also runs later entries.
PROMPT_COMMAND="__vtcode_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
