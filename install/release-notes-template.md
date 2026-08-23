<details open>
<summary><b>🇨🇳 简体中文</b></summary>

### Teak CLI v0.0.3

- **`--resume` / `/new` 后会话 ID 会换成正在跑的那条。** 复制 ID、重启恢复不再指向 fork 前的旧对话。
- **Claude 会话名优先用 CLI 的 ai-title。** 左侧栏跟 Claude 自己的标题对齐，不再钉死第一句用户提示。
- **后台恢复的 tab 等有真实尺寸再启动 PTY。** 避免 Grok/Claude TUI 在 0×0 容器里画完再也撑不满。
- **打开中的会话会记住终端 / 对话视图。** 重启后回到离开时的视图。

</details>

<details>
<summary><b>🇬🇧 English</b></summary>

### Teak CLI v0.0.3

- **Session IDs update after `--resume` / `/new`.** Copy-id and restore now target the conversation that is actually running, not the pre-fork greeting.
- **Claude labels prefer the CLI's ai-title.** The left rail matches Claude's own title instead of freezing the first user prompt.
- **Restored background tabs wait for a real terminal size before starting the PTY.** Grok/Claude TUIs no longer paint into a 0×0 pane and stay tiny.
- **Open sessions remember terminal vs chat view.** A restart returns to the view you left.

</details>

<details>
<summary><b>🇹🇼 繁體中文</b></summary>

### Teak CLI v0.0.3

- **`--resume` / `/new` 後工作階段 ID 會換成正在跑的那條。** 複製 ID、重啟恢復不再指向 fork 前的舊對話。
- **Claude 工作階段名優先用 CLI 的 ai-title。** 左側欄跟 Claude 自己的標題對齊，不再釘死第一句使用者提示。
- **背景恢復的 tab 等有真實尺寸再啟動 PTY。** 避免 Grok/Claude TUI 在 0×0 容器裡畫完再也撐不滿。
- **開啟中的工作階段會記住終端機 / 對話檢視。** 重啟後回到離開時的檢視。

</details>

<details>
<summary><b>🇯🇵 日本語</b></summary>

### Teak CLI v0.0.3

- **`--resume` / `/new` のあと、セッション ID は実行中の会話に更新されます。** コピーや復元が fork 前の挨拶セッションを指しません。
- **Claude の表示名は CLI の ai-title を優先します。** 最初のユーザー発話で固定しません。
- **復元したバックグラウンドタブは実サイズになってから PTY を起動します。** 0×0 のまま小さく描画されません。
- **開いているセッションはターミナル / チャット表示を覚えています。**

</details>

<details>
<summary><b>🇰🇷 한국어</b></summary>

### Teak CLI v0.0.3

- **`--resume` / `/new` 이후 세션 ID가 실제로 돌아가는 대화로 바뀝니다.** 복사·복원이 fork 이전 인사 세션을 가리키지 않습니다.
- **Claude 이름은 CLI ai-title을 우선합니다.** 첫 사용자 프롬프트에 고정되지 않습니다.
- **복원된 백그라운드 탭은 실제 크기가 난 뒤에 PTY를 시작합니다.** 0×0에서 작게 그려지지 않습니다.
- **열린 세션은 터미널 / 채팅 화면을 기억합니다.**

</details>
