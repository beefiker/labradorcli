brew install tmux

if test $status -eq 0
    tmux -Llabrador -CC
    exit
end
