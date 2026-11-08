@echo off
REM Agent2API 一键启动：拉起 WSL（顺带 docker 与容器）并常驻，避免 WSL 被回收
REM 用法：双击或在 cmd 里运行

echo [1/3] 启动 WSL 常驻进程...
start "" /min wsl.exe -d Ubuntu -u root --exec /bin/sh -c "exec sleep infinity"

echo [2/3] 等待 Docker 就绪...
timeout /t 8 /nobreak >nul

echo [3/3] 启动容器 agent2api-dev...
wsl.exe -d Ubuntu -u root --exec docker start agent2api-dev >nul 2>&1

echo.
echo 完成。面板地址： http://localhost:3065
echo 若仍打不开，等 15 秒再刷新一次（网关初始化要几秒）。
echo.
pause
