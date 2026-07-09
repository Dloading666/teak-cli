<details open>
<summary><b>🇨🇳 简体中文</b></summary>

- 新增:修改记录里的文件比对,点放大不再全屏遮住 agent,而是开成中间一个 tab——和 Claude/Codex 那些 tab 同级,看 diff 时 agent 还在跑、看得见。tab 内顶部显示文件完整路径,右侧图标点一下缩回右下角小窗,关闭走 tab 自带的 ×。偏好会记住,下次直接进 tab。
- 新增:Codex 的灵动岛现在有完整三色了——干活时橙、请求权限时蓝、闲着绿(原来只有绿)。给 Codex 装了完整的 hook(SessionStart/UserPromptSubmit/PermissionRequest/Stop),状态跟 Claude 一样实时变。首次使用 Codex 可能需要在它的 /hooks 里批准一下我们的 hook 条目(这是 Codex 自己的安全门,我们不替你按)。
- 修复:用 Coffee CLI 启动 opencode 后,如果 opencode 自己升级过,它的命令可能从系统里消失(`'opencode' 不是内部或外部命令`)。根因是 opencode 升级重装时撞上 Windows 文件锁,把全局 bin 链接写断了。现在 Coffee CLI 启动时会检测到这种断链并自动重装修复,你不用再手动 npm install。

</details>

<details>
<summary><b>🇹🇼 繁體中文</b></summary>

- 新增:修改記錄裡的檔案比對,點放大不再全螢幕遮住 agent,而是開成中間一個 tab——和 Claude/Codex 那些 tab 同級,看 diff 時 agent 還在跑、看得見。tab 內頂部顯示檔案完整路徑,右側圖示點一下縮回右下角小窗,關閉走 tab 自帶的 ×。偏好會記住,下次直接進 tab。
- 新增:Codex 的靈動島現在有完整三色了——幹活時橙、請求權限時藍、閒著綠(原來只有綠)。給 Codex 裝了完整的 hook(SessionStart/UserPromptSubmit/PermissionRequest/Stop),狀態跟 Claude 一樣即時變。首次使用 Codex 可能需要在它的 /hooks 裡批准一下我們的 hook 條目(這是 Codex 自己的安全門,我們不替你按)。
- 修復:用 Coffee CLI 啟動 opencode 後,如果 opencode 自己升級過,它的命令可能從系統裡消失(`'opencode' 不是內部或外部命令`)。根因是 opencode 升級重裝時撞上 Windows 檔案鎖,把全域 bin 連結寫斷了。現在 Coffee CLI 啟動時會偵測到這種斷鏈並自動重裝修復,你不用再手動 npm install。

</details>

<details>
<summary><b>🇬🇧 English</b></summary>

- New: Expanding a file diff from the Changes panel no longer opens a full-screen modal that hides your agent — it opens as a center tab, a peer of the Claude/Codex tabs, so the agent keeps running and stays visible while you read the diff. The tab's header shows the file's full path; the icon on the right folds the diff back to the bottom-right overlay, and the tab's own × closes it. Your choice is remembered — next time it opens straight into the tab.
- New: Codex's dynamic-island indicator now has all three colors — orange while working, blue on permission requests, green when idle (previously only green). We install Codex's full hook set (SessionStart / UserPromptSubmit / PermissionRequest / Stop), so the status changes live like Claude's. On first use Codex may ask you to approve our hook entries in its /hooks screen — that's Codex's own trust gate, we don't bypass it for you.
- Fixed: After launching opencode through Coffee CLI, if opencode had upgraded itself, its command could vanish from the system (`'opencode' is not recognized`). The root cause: opencode's upgrade reinstall collides with a Windows file lock and severs the global bin links. Coffee CLI now detects this breakage at launch and auto-runs the repair install — no manual `npm install` needed.

</details>

<details>
<summary><b>🇯🇵 日本語</b></summary>

- 新機能:変更記録のファイル比較で、拡大ボタンを押すと agent を隠す全画面モーダルではなく中央のタブとして開くようになりました(Claude/Codex のタブと同列)。diff を見ながら agent が動き続け、見えたまま。タブ上部にはファイルの完全パスを表示、右のアイコンで右下のオーバーレイに縮小、閉じるはタブ自带の ×。設定は記憶され、次回は直接タブで開きます。
- 新機能:Codex のタブステータス表示が三色すべて対応しました——作業中はオレンジ、権限要求でブルー、待機中はグリーン(以前はグリーンのみ)。Codex のフルフック(SessionStart / UserPromptSubmit / PermissionRequest / Stop)をインストールし、Claude と同様にリアルタイムでステータスが変化します。初回利用時、Codex の /hooks で当社のフックを承認してもらう場合があります(Codex 自身のセキュリティゲートなので代理で押しません)。
- 修正:Coffee CLI から opencode を起動した後、opencode 自身がアップグレードしているとコマンドがシステムから消えることがありました(`'opencode' は内部コマンドまたは外部コマンドとして認識されていません`)。原因は opencode のアップグレード再インストールが Windows のファイルロックと衝突し、グローバル bin リンクを切断するためです。Coffee CLI は起動時にこの破損を検出して自動修復インストールを実行するようになり、手動 npm install は不要です。

</details>

<details>
<summary><b>🇰🇷 한국어</b></summary>

- 신규:변경 기록의 파일 비교에서 확대 버튼을 누르면 agent를 가리는 전체화면 모달 대신 중앙 탭으로 열립니다(Claude/Codex 탭과 동일 선상). diff를 보는 동안 agent는 계속 돌고 보입니다. 탭 상단에는 파일 전체 경로를 표시하고, 오른쪽 아이콘으로 우하단 오버레이로 축소, 닫기는 탭 자체 ×로. 선택은 기억되어 다음에는 곧바로 탭으로 열립니다.
- 신규:Codex의 탭 상태 표시가 세 색 모두 지원됩니다——작업 중 주황, 권한 요청 파랑, 대기 초록(이전에는 초록만). Codex의 전체 훅(SessionStart / UserPromptSubmit / PermissionRequest / Stop)을 설치하여 Claude처럼 상태가 실시간으로 변합니다. 최초 사용 시 Codex의 /hooks에서 당사의 훅 항목을 승인하라는 안내가 나올 수 있습니다(Codex 자체 보안 게이트라 대신 눌러드리지 않습니다).
- 수정:Coffee CLI에서 opencode를 실행한 후 opencode가 자체 업그레이드했다면 명령이 시스템에서 사라질 수 있었습니다(`'opencode'은(는) 내부 또는 외부 명령으로 인식되지 않습니다`). 원인은 opencode 업그레이드 재설치가 Windows 파일 잠금과 충돌하여 전역 bin 링크를 끊어놓기 때문입니다. Coffee CLI는 실행 시 이 손상을 감지해 자동 복구 설치를 실행하므로, 더 이상 수동 npm install이 필요 없습니다.

</details>
