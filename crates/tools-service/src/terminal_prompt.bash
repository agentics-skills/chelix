__chelix_status=$?
builtin set +H
: "${__chelix_prompt_id:=0}"
if [[ ! ${__chelix_ps1+x} || ${__chelix_ps1-} != "$PS1" ]]; then
    PS1='\[\e]633;P;ChelixPromptId=$((__chelix_prompt_id += 1))\a\e]633;A\a\]'"$PS1"'\[\e]633;B\a\]'
    __chelix_ps1=$PS1
fi
if [[ ! ${__chelix_ps2+x} || ${__chelix_ps2-} != "$PS2" ]]; then
    PS2='\[\e]633;P;ChelixPromptId=$((__chelix_prompt_id += 1))\a\e]633;F\a\]'"$PS2"'\[\e]633;G\a\]'
    __chelix_ps2=$PS2
fi
builtin printf '\033]633;D;%s\007' "$__chelix_status"
builtin trap 'builtin trap - DEBUG; builtin printf "\033]633;C\007"' DEBUG
