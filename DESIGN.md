Codex TUI 整体是三段式垂直布局：

┌─────────────────────────────────────────┐
│ Header: cwd / model / session 信息        │  1 行，可选
├─────────────────────────────────────────┤
│                                           │
│  Transcript / Chat 滚动区 (可占满)         │  自适应高度
│  (HistoryCell 列表：用户消息/模型回复/      │
│   工具调用卡片/diff/错误)                  │
│                                           │
├─────────────────────────────────────────┤
│ Status line: [● running] shimmer / 提示    │  1 行，条件显示
├─────────────────────────────────────────┤
│ BottomPane:                              │
│  ┌─ ChatComposer (多行输入框) ─────────┐  │
│  │ > 用户正在输入...                   │  │
│  └──────────────────────────────────┘  │
│  footer: 快捷键提示 / model hint          │  1 行
└─────────────────────────────────────────┘

实现要点：


用 Layout::vertical([Constraint::Min(0), Constraint::Length(composer_height)]) 分割，composer 高度要动态计算（根据输入内容行数 + 边框，通常 3~8 行可变，这是 Codex 手感的关键之一：单行输入时矮，多行粘贴时自动长高）。
Transcript 区域用 Constraint::Min(0) 吃掉剩余空间，永远优先保证聊天区域最大化。
每次有新内容或 tick 都要重新计算这个动态高度并触发 redraw。



3. Transcript / 滚动聊天区 —— HistoryCell 模式

不要用一个大 String + Paragraph::scroll() 硬滚，Codex 的做法更像"每条消息是一个独立组件"：

rustpub trait HistoryCell {
    fn desired_height(&self, width: u16) -> u16;
    fn render(&self, area: Rect, buf: &mut Buffer, ctx: &RenderContext);
    fn is_streaming(&self) -> bool { false }
}


ChatWidget 持有 Vec<Box<dyn HistoryCell>>（已完成的历史）+ 一个可变的 active_cell: Option<Box<dyn HistoryCell>>（正在流式输出、还没定型的那条）。
滚动逻辑：先算出所有 cell 的 desired_height 累加总高度，viewport 只渲染可见范围内的 cell（简单虚拟滚动），滚动位置存一个 scroll_offset: u16，PageUp/PageDown/鼠标滚轮改这个值。
不要每帧重新排版全部历史：把已经渲染完的历史 cell 结果缓存高度（layout cache），只有窗口宽度变化时才需要重新计算 desired_height。
Cell 类型至少要有：UserMessageCell、AgentMessageCell（支持流式追加+markdown）、ToolCallCell（见第 5 节）、DiffCell（语法高亮的代码 diff）、ErrorCell、ApprovalRequestCell。
全屏"转录浏览"模式（对应 Codex 的 Ctrl+T）：一个 overlay，把当前 transcript 用只读方式全屏展示，方便复制/搜索。



4. 输入框 (ChatComposer / Input Bar)

这是最容易做得"很糙"的地方，细节决定手感：

必须实现的行为：


多行文本编辑：维护 lines: Vec<String> + cursor: (row, col)，支持左右上下移动、Home/End、Ctrl+A/E（emacs 风格）、Backspace/Delete 跨行合并。
Tab 排队 (queueing)：Codex 里模型还在跑的时候按 Tab，会把当前输入排队到下一轮而不是立即发送——这是"感觉很聪明"的细节，很多简单实现会漏掉。
历史导航：Up/Down 在光标位于首行/末行且没有多行内容时，翻阅之前发送过的 prompt（存一个 Vec<String> history + history_index）。
Ctrl+R 反向搜索历史（类似 shell 的 reverse-i-search）。
粘贴检测：用 crossterm 的 bracketed-paste feature，多行粘贴时不要逐字符触发自动补全/高度重算抖动，整块插入后统一 relayout 一次。
图片/文件占位符：拖拽或粘贴截图时，输入框里显示一个 [image #1] 占位 token，而不是把 base64 塞进文本。


边框状态反馈（这也是"高级感"的重要来源）：


空闲态：细边框，暗色。
聚焦输入态：边框高亮色（Codex 用品牌色，比如 magenta/cyan）。
模型正在运行态：边框可以配合 shimmer 或者显示一个 spinner + "Esc 中断" 提示。


示例骨架：

rustpub struct ChatComposer {
    lines: Vec<String>,
    cursor: (usize, usize),
    history: Vec<String>,
    history_index: Option<usize>,
    pending_queue: VecDeque<QueuedInput>, // Tab 排队
    mode: ComposerMode,                    // Normal | SlashMenu | HistorySearch
}

impl ChatComposer {
    fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
        match (key.code, self.mode) {
            (KeyCode::Char('/'), ComposerMode::Normal) if self.cursor_at_line_start_or_after_space() => {
                self.mode = ComposerMode::SlashMenu;
                ComposerAction::OpenSlashMenu
            }
            (KeyCode::Tab, _) if self.is_agent_running => {
                ComposerAction::QueueForNextTurn
            }
            (KeyCode::Enter, ComposerMode::Normal) if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                ComposerAction::Submit(self.take_text())
            }
            // Shift+Enter / Alt+Enter -> 插入换行而不提交
            ...
        }
    }
}


5. Slash Commands (/model, /clear, /theme ...)

实现为 BottomPane 之上的一个 overlay popup，不要和输入框逻辑耦合死：


用户输入 / 时（且是在行首），弹出一个悬浮列表，渲染在输入框正上方（Rect 计算：composer_area.y - popup_height）。
列表内容 = 静态命令表 + 模糊匹配当前输入的剩余字符做前缀/子序列过滤（类似 fzf 的简单子序列匹配即可，不需要真做 fzf 算法）。
Up/Down 选中，Enter 确认，Esc 关闭。
确认后：如果命令需要参数（比如 /model 要弹出模型选择二级菜单），转到子 overlay；否则直接分发一个 AppEvent::SlashCommand(cmd) 给 App 处理。


rustpub struct SlashCommandPopup {
    all_commands: &'static [SlashCommand],
    query: String,
    selected: usize,
}

impl SlashCommandPopup {
    fn filtered(&self) -> Vec<&SlashCommand> {
        self.all_commands.iter()
            .filter(|c| fuzzy_subsequence_match(&self.query, c.name))
            .collect()
    }
}

命令建议至少覆盖：/model（切换模型/推理等级）、/clear（清屏+新会话）、/theme、/copy、/help、/exit、/compact（手动触发上下文压缩）。


6. Model Hint / 状态栏

Codex 在 footer 或 header 常驻显示：当前 model 名、审批模式 (untrusted / on-request / full-access)、cwd、以及一个 token/context 用量提示。做法：


一行 Line，用 Span 分段上色：Span::styled(" gpt-5.2-codex ", model_style) + 分隔符 │ + Span::styled("workspace-write", mode_style)。
当上下文快接近上限时（比如 >80%），把这个 span 变成警示色（黄/红），并在旁边加百分比。
输入框为空且未聚焦时，可以在 composer 内部用灰色 placeholder 文本给出提示（"输入 / 查看命令，Shift+Enter 换行"），这是很多简单实现会漏掉的"品牌感"来源。



7. Function Calling / 工具调用展示

这是 Codex 手感里最关键、最容易做"naive"的部分。不要把工具调用当成一段普通文本打印，要做成结构化、可折叠、状态可变的卡片：

rustpub struct ToolCallCell {
    pub tool_name: String,
    pub args_preview: String,       // 单行摘要，比如 "read_file(src/main.rs)"
    pub status: ToolStatus,          // Pending | Running | Success | Failed
    pub output: Option<String>,      // 展开后显示的完整 stdout/结果
    pub expanded: bool,
    pub diff: Option<UnifiedDiff>,   // 如果是文件编辑，存 diff 结构而不是纯文本
}

pub enum ToolStatus { Pending, Running, Success(Duration), Failed(String) }

渲染规则：


头部一行：图标/前缀（▸/●/✓/✗）+ 工具名 + 参数摘要，颜色随 status 变化（Running=cyan 带 shimmer，Success=green，Failed=red）。
Running 状态：整行文字用 shimmer 效果扫光（见第 8 节），给用户"正在执行"的实时反馈，而不是死的 spinner。
折叠态默认只显示一行摘要；展开态（比如按 Enter 或自动在失败时展开）显示完整输出，长输出要截断+"查看更多"提示，不要一次性把几千行都塞进 transcript。
Diff 渲染：文件编辑类工具调用，不要展示 raw unified diff 文本，要逐行上色（新增绿底/删除红底/hunk header 灰色加粗），并且做语法高亮（可用 syntect 或轻量 tree-sitter 高亮）。
审批流：如果工具调用需要用户审批，渲染成一个交互式 overlay（Y/N/Always/编辑后再执行 四个选项），阻塞输入焦点直到用户决定，决定后这个 overlay 消失，ToolCallCell 状态变成 Running。
把连续的多个工具调用（比如模型一次性读了 5 个文件）合并/分组显示，而不是 5 张独立大卡片刷屏——这是 codex-rs 里 active_cell 常常"合并 exec/tool 组"的原因。



8. Shimmer / Loading 动画

Codex 的标志性效果之一：文字上有一道高光从左到右扫过，循环播放，用在"Thinking..."/工具执行中的状态提示上。

原理：不是逐字符切换颜色的随机效果，而是基于时间的相位（phase）计算每个字符到高亮中心的距离，距离越近颜色越亮，形成一个移动的"渐变波峰"。

rust// 极简实现思路（也可以直接用 tui-shimmer crate）
fn shimmer_spans(text: &str, base: Style, phase: f32) -> Vec<Span<'static>> {
    let len = text.chars().count().max(1) as f32;
    text.chars().enumerate().map(|(i, ch)| {
        let pos = i as f32 / len;                  // 0..1
        let dist = (pos - phase).abs().min(1.0 - (pos - phase).abs()); // 环形距离
        let highlight = (1.0 - dist * 4.0).clamp(0.0, 1.0); // 高光衰减
        let color = blend(base.fg.unwrap_or(Color::Gray), Color::White, highlight);
        Span::styled(ch.to_string(), base.fg(color))
    }).collect()
}


phase 用一个全局 Instant 算：phase = (start.elapsed().as_secs_f32() / period).rem_euclid(1.0)，period 建议 1.5~2 秒扫一圈。
不要用 thread::sleep 驱动动画，要挂在主 tick（比如 tokio::time::interval(Duration::from_millis(50))，即 ~20fps 足够丝滑，不需要真 60fps）触发 redraw，phase 是纯函数算出来的，不需要额外状态机。
真彩终端做 RGB 插值；检测到只支持 256 色/16 色时，降级成"加粗+灰阶"跑马灯，直接用 tui-shimmer crate 就自带这个 fallback，不用自己写。
应用位置：状态栏的 "Thinking…" 文字、ToolCallCell 处于 Running 状态时的一行摘要文字。只对单行短文本用 shimmer，不要整段大文字都扫光，观感会很吵。


其他动画建议：


Spinner：throbber-widgets-tui 或自己用 braille 字符 (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) 按 tick 轮换，用在"正在等待模型响应"但还没有文字流出来的阶段。
新 cell 出现时可以加一个轻微的 fade-in（用 tachyonfx 的 fade_from），让新增内容不那么突兀，但这是锦上添花，优先级低于上面几项。



9. 流式文本渲染 (Streaming)

模型输出是逐 token 流式过来的，不能等全部内容到齐再渲染：


active_cell 持有一个可增量 append 的 buffer；每次收到新 delta，append 到 buffer，然后只重新解析/渲染这一个 cell（markdown 增量解析可以简单粗暴：每次都重新跑一遍全量 markdown parser，只要文本量不是几万字符，性能完全够用，不需要真正的增量 parser）。
Markdown → styled Text：至少支持标题、粗体/斜体、行内 code、代码块（配语法高亮）、列表、链接（渲染成带下划线的文字，OSC 8 超链接可选）。
打字机效果（可选）：如果想要逐字"打出来"的效果而不是一次性刷入一大段，维护一个"已显示到第几个字符"的指针，每个 tick 往前推进 N 个字符，直到追上实际 buffer 长度；这个效果要能被真实流速"追上"（如果模型出得快，指针要能跳跃式追平，不要人为限速卡顿）。



10. 主事件循环骨架

rustasync fn run(mut app: App) -> Result<()> {
    let mut term_events = crossterm::event::EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50)); // 驱动动画/shimmer

    loop {
        tokio::select! {
            Some(Ok(ev)) = term_events.next() => {
                app.handle_terminal_event(ev)?;
            }
            Some(protocol_ev) = app.protocol_rx.recv() => {
                app.chat_widget.handle_protocol_event(protocol_ev);
            }
            _ = tick.tick() => {
                app.on_tick(); // 推进 shimmer phase / spinner frame / 打字机指针
            }
        }
        if app.needs_redraw() {
            terminal.draw(|f| app.render(f))?;
        }
        if app.should_quit { break; }
    }
    Ok(())
}

要点：


三路事件（终端输入 / 后端协议事件 / 定时 tick）都汇聚到一个 select!，谁都不阻塞谁。
用一个 dirty: bool 或者更细粒度的脏区标记，避免没变化时也重绘（省 CPU，也避免终端"闪烁感"）。
后端 agent loop（真正调用模型 API、跑工具）要放在单独的 tokio task 里，通过 mpsc channel 把事件发回 UI 线程，UI 绝对不能同步阻塞等模型返回。



11. 给 agent 的实施顺序建议

按这个顺序实现，每一步都能跑起来看到效果，避免"写到一半发现骨架不对要重来"：


骨架：App + 主 tick 循环 + 空的三段布局，能正常进入/退出 alternate screen。
输入框：多行编辑 + 提交 + 历史导航（先不做 Tab 排队）。
HistoryCell + 滚动：UserMessageCell / AgentMessageCell 先跑通，接一个假的 mock 事件源验证滚动、动态高度。
接后端：换成真实的 agent loop（模型 API 流式调用），把 protocol event 接进 ChatWidget。
Slash commands：/model /clear /help 先做，验证 popup + 分发逻辑。
工具调用卡片：ToolCallCell + 折叠/展开 + diff 渲染 + 审批 overlay。
状态栏 + model hint：footer/header 信息条。
动画层：shimmer（先接 tui-shimmer crate 别自己重造）、spinner、打字机。
打磨：Tab 排队、Ctrl+R 历史搜索、转录全屏浏览、主题切换、图片粘贴占位符。


一句话给 agent 的提醒：视觉上"像不像 Codex"90% 取决于第 3、6、8 步（HistoryCell 分层、工具调用卡片化、shimmer 动画），如果时间有限优先把这三项做扎实，其余是锦上添花。
