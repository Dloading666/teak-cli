<details open>
<summary><b>🇨🇳 简体中文</b></summary>

### AI 终端，从此有了第二种更舒服的打开方式

- **v3.4.4：思考与执行，现在有了连续而准确的状态反馈。** Agent 开始调用工具或运行命令时，泡泡对话会把六点动画旁的「正在思考中…」切换为「正在执行中…」；提示仅跟随当前活跃回合，执行结束、切换回合或 Agent 空闲后会及时消失，不会被历史工具记录误触发。
- **11 种语言保持完整同步，并新增编译期防漏机制。** 「正在执行中…」已覆盖全部支持语言；同时补齐了既有 Gambit 文案。现在每份语言包都严格包含相同的 164 个翻译键，任何漏译、多余键或拼写错误都会让 TypeScript 构建直接失败。
- **v3.4.3：泡泡对话现在会完整跟随界面语言。** 「正在思考中」「思考过程」、复制按钮的读屏提示，以及对话快速定位与轮次跳转提示，现已覆盖简体中文、繁體中文、English、日本語、한국어、Español、Français、Deutsch、Português、Русский 和 Tiếng Việt。与此同时，前端 238 个 ESLint 错误与 2 个警告已全部清零，翻译键也恢复严格类型检查。
- **需要精准控制时用终端，想专注交流时用泡泡对话。** 现在可以直接从 Gambit 的统一选择器，在原生终端与全新「泡泡对话」之间切换。对话界面完整跟随 Coffee CLI 的主题、形态、材质与壁纸；Gambit 仍然是真正的可拖动分屏，不会再遮住最后一条回复。
- **再长的对话，也能保持轻盈丝滑。** 最近消息分页、可变高度虚拟列表、增量解析、Markdown 记忆化渲染，以及工具与思考内容的按需展开共同工作；即使面对数百条消息和巨大的工具输出，打字、滚动、拖动高度依然流畅。
- **一眼看完整段旅程，一点回到任意问题。** 打开历史记录时，左侧快速定位会立即建立整段会话的提问索引。悬停即可看到主题化预览，点击任意短线便会自动加载需要的旧分页并精准抵达，无需先手动向上翻找。
- **不是换层皮，而是一套完整的对话体验。** 细腻的思考动画、每轮总结与用户消息的复制按钮、对号反馈、与终端一致的「复制 / 粘贴 / 全选」右键菜单、壁纸支持，以及 SQLite / JSONL 实时同步，让泡泡模式真正成为可以长期工作的主界面。

</details>

<details>
<summary><b>🇬🇧 English</b></summary>

### A new way to work with AI terminals

- **v3.4.4: Thinking and execution now have continuous, accurate status feedback.** When the agent starts a tool call or command, Bubble Conversation switches the six-dot activity label from “Thinking…” to “Executing…”. The indicator is scoped to the active turn and clears when execution finishes, the turn changes or the agent becomes idle, so historical tool records cannot reactivate it.
- **All 11 languages now stay structurally in sync.** “Executing…” is localized everywhere, and previously missing Gambit strings have been completed. Every locale now contains the same 164 translation keys, with compile-time checks that fail the TypeScript build on missing, extra or misspelled keys.
- **v3.4.3: Bubble Conversation now follows the selected interface language throughout.** Thinking status, reasoning labels, copy-button accessibility text, conversation navigation and turn-jump announcements are localized across all 11 supported languages. This release also clears all 238 frontend ESLint errors and 2 warnings and restores strict typing for translation keys.
- **Terminal when you need precision, conversation when you want clarity.** Switch any AI CLI between the raw terminal and the new Bubble Conversation view from Gambit's unified selector. The conversation follows your Coffee CLI theme, shape, material and wallpaper, while Gambit remains a true resizable split instead of covering the latest reply.
- **Long conversations now feel light.** Recent-tail paging, variable-height virtualization, incremental parsing, memoized Markdown and lazy tool/reasoning details keep typing, scrolling and resizing smooth—even in sessions with hundreds of messages and large tool outputs.
- **Jump through an entire session instantly.** The new prompt rail indexes the complete conversation as soon as history opens. Hover for a themed preview, then click any marker: Coffee CLI loads the required older pages and lands on that question without making you scroll upward first.
- **Conversation-native polish throughout.** A polished thinking animation, per-turn copy actions with checkmark feedback, terminal-style Copy / Paste / Select All context menus, background wallpaper support and live SQLite/JSONL synchronization make the Bubble view feel like a first-class workspace—not a terminal transcript wearing a new skin.

</details>

<details>
<summary><b>🇹🇼 繁體中文</b></summary>

### AI 終端，從此有了第二種更舒服的開啟方式

- **v3.4.4：思考與執行，現在有了連續而準確的狀態回饋。** Agent 開始呼叫工具或執行命令時，泡泡對話會把六點動畫旁的「正在思考中…」切換為「正在執行中…」；提示僅跟隨目前的活動回合，執行結束、切換回合或 Agent 閒置後會即時消失，不會被歷史工具記錄誤觸發。
- **11 種語言保持完整同步，並新增編譯期防漏機制。** 「正在執行中…」已涵蓋所有支援語言，同時補齊既有 Gambit 文案。現在每份語言包都嚴格包含相同的 164 個翻譯鍵，任何漏譯、多餘鍵或拼寫錯誤都會讓 TypeScript 建置直接失敗。
- **v3.4.3：泡泡對話現在會完整跟隨介面語言。** 「正在思考中」「思考過程」、複製按鈕的讀屏提示，以及對話快速定位與輪次跳轉提示，現已涵蓋全部 11 種支援語言。本次版本也清除了前端全部 238 個 ESLint 錯誤與 2 個警告，並恢復翻譯鍵的嚴格型別檢查。
- **需要精準控制時用終端，想專注交流時用泡泡對話。** 現在可以直接從 Gambit 的統一選擇器，在原生終端與全新「泡泡對話」之間切換。對話介面完整跟隨 Coffee CLI 的主題、形態、材質與桌布；Gambit 仍然是真正可拖曳的分割畫面，不會再蓋住最後一則回覆。
- **再長的對話，也能保持輕盈流暢。** 最近訊息分頁、可變高度虛擬列表、增量解析、Markdown 記憶化渲染，以及工具與思考內容的按需展開共同運作；即使面對數百則訊息和龐大的工具輸出，打字、捲動、拖曳高度依然順暢。
- **一眼看完整段旅程，一點回到任意問題。** 開啟歷史記錄時，左側快速定位會立即建立整段會話的提問索引。懸停即可看到主題化預覽，點擊任意短線便會自動載入需要的舊分頁並精準抵達，不必先手動向上翻找。
- **不是換層外觀，而是一套完整的對話體驗。** 細緻的思考動畫、每輪總結與使用者訊息的複製按鈕、勾選回饋、與終端一致的「複製 / 貼上 / 全選」右鍵選單、桌布支援，以及 SQLite / JSONL 即時同步，讓泡泡模式真正成為可以長期工作的主介面。

</details>

<details>
<summary><b>🇯🇵 日本語</b></summary>

### AI ターミナルに、もっと心地よいもう一つの使い方

- **v3.4.4：思考と実行の状態を、途切れず正確に確認できるようになりました。** Agent がツール呼び出しやコマンド実行を始めると、6 点アニメーションの表示が「考え中…」から「実行中…」へ切り替わります。表示は現在進行中のターンだけに限定され、実行完了、ターン切り替え、待機状態への移行時に消えるため、過去のツール履歴で再表示されることはありません。
- **11 言語を完全に同期し、翻訳漏れをビルド時に検出します。** 「実行中…」をすべての対応言語に追加し、既存の Gambit 文言の不足も補完しました。各言語ファイルは同じ 164 キーを持ち、不足・余分・タイプミスがある場合は TypeScript ビルドが失敗します。
- **v3.4.3：バブル会話が選択中の表示言語に完全対応しました。** 思考中ステータス、思考プロセス、コピー操作のアクセシビリティ文言、会話ナビゲーション、質問へのジャンプ案内を、対応する全 11 言語で表示します。フロントエンドに残っていた ESLint の 238 エラーと 2 警告もすべて解消し、翻訳キーの厳密な型チェックを復元しました。
- **正確な操作にはターミナル、会話に集中したいときにはバブル表示。** Gambit の一体型セレクターから、従来のターミナルと新しい「バブル会話」をすぐに切り替えられます。テーマ、シェイプ、マテリアル、壁紙をそのまま引き継ぎ、Gambit も最新の返答を覆わない本当のリサイズ可能な分割表示として動作します。
- **長い会話も、軽く滑らかに。** 直近メッセージのページング、可変高さの仮想リスト、差分パース、Markdown のメモ化、ツール／思考内容の遅延表示により、数百件のメッセージや巨大なツール出力があっても、入力・スクロール・リサイズを快適に保ちます。
- **会話全体を見渡し、ワンクリックで過去の質問へ。** 履歴を開くと、左側のクイックナビがセッション全体の質問インデックスをすぐに作成します。ホバーでテーマに合ったプレビューを確認し、マーカーをクリックするだけで必要な過去ページを読み込み、目的の質問へ正確に移動できます。
- **見た目だけではない、完成された会話体験。** 洗練された思考アニメーション、各ターンのコピーとチェック表示、ターミナル共通の「コピー／貼り付け／すべて選択」メニュー、壁紙、SQLite / JSONL のライブ同期まで揃え、バブル表示を本格的な作業画面に仕上げました。

</details>

<details>
<summary><b>🇰🇷 한국어</b></summary>

### AI 터미널을 더 편안하게 사용하는 새로운 방식

- **v3.4.4: 사고와 실행 상태를 끊김 없이 정확하게 보여줍니다.** Agent가 도구 호출이나 명령 실행을 시작하면 6점 애니메이션 옆 문구가 “생각 중…”에서 “실행 중…”으로 바뀝니다. 표시는 현재 활성 턴에만 적용되며 실행 완료, 턴 전환 또는 Agent 대기 상태에서 사라지므로 이전 도구 기록 때문에 다시 나타나지 않습니다.
- **11개 언어를 완전히 동기화하고 번역 누락을 빌드 단계에서 차단합니다.** “실행 중…”을 모든 지원 언어에 추가하고 기존 Gambit 문구의 누락도 보완했습니다. 이제 각 언어 파일은 동일한 164개 키를 가지며 누락, 불필요한 키 또는 오타가 있으면 TypeScript 빌드가 실패합니다.
- **v3.4.3: 버블 대화가 선택한 인터페이스 언어를 완전히 따릅니다.** 생각 중 상태, 사고 과정, 복사 버튼의 접근성 문구, 대화 탐색 및 질문 이동 안내를 지원되는 11개 언어로 제공합니다. 프런트엔드에 남아 있던 ESLint 오류 238개와 경고 2개도 모두 해결하고 번역 키의 엄격한 타입 검사를 복원했습니다.
- **정밀한 제어가 필요할 때는 터미널, 대화에 집중하고 싶을 때는 버블 화면.** Gambit의 통합 선택기에서 기존 터미널과 새로운 버블 대화 화면을 즉시 전환할 수 있습니다. Coffee CLI의 테마, 형태, 소재, 배경화면을 그대로 따르며, Gambit은 마지막 답변을 가리지 않는 실제 크기 조절 분할 화면으로 동작합니다.
- **아무리 긴 대화도 가볍고 부드럽게.** 최근 메시지 페이지 로딩, 가변 높이 가상 목록, 증분 파싱, Markdown 메모이제이션, 도구 및 사고 과정의 지연 렌더링을 결합해 수백 개의 메시지와 큰 도구 출력에서도 입력·스크롤·크기 조절이 매끄럽습니다.
- **전체 대화를 한눈에 보고, 한 번의 클릭으로 원하는 질문으로.** 기록을 열면 왼쪽 빠른 탐색 막대가 전체 세션의 질문 인덱스를 즉시 만듭니다. 테마에 맞는 미리보기를 확인한 뒤 표시를 클릭하면 필요한 이전 페이지를 자동으로 불러와 정확한 질문 위치로 이동합니다.
- **단순히 모양만 바꾼 것이 아닌 완성된 대화 경험.** 세련된 사고 애니메이션, 턴별 복사와 체크 표시, 터미널과 동일한 복사 / 붙여넣기 / 전체 선택 메뉴, 배경화면 지원, SQLite / JSONL 실시간 동기화까지 갖춰 버블 화면을 장시간 작업할 수 있는 진정한 메인 인터페이스로 만들었습니다.

</details>
