#!/bin/bash

SESSION_NAME="MONITOR"

# Xóa session MONITOR cũ nếu đang chạy để làm mới
tmux kill-session -t $SESSION_NAME 2>/dev/null

# Tạo session mới và attach vào session go-master ở ô đầu tiên
tmux new-session -d -s $SESSION_NAME "unset TMUX; tmux attach -t go-master"

# Chia đôi màn hình theo chiều dọc và attach go-master-1
tmux split-window -h -t $SESSION_NAME "unset TMUX; tmux attach -t go-master-1"

# Chia đôi ô bên trái theo chiều ngang và attach go-sub
tmux split-window -v -t $SESSION_NAME.0 "unset TMUX; tmux attach -t go-sub"

# Chia đôi ô bên phải theo chiều ngang và attach go-sub-1
tmux split-window -v -t $SESSION_NAME.1 "unset TMUX; tmux attach -t go-sub-1"

# Cân bằng lại kích thước các ô
tmux select-layout -t $SESSION_NAME tiled

# Truy cập vào session tổng hợp này
tmux attach-session -t $SESSION_NAME