import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Window } from '../stageshow/kit/window'
import { Button } from '../stageshow/kit/controls/Button'
import { Segmented } from '../stageshow/kit/controls/Segmented'
import { useSiteTheme } from './theme'
import './demos.css'

// Freeform Notion/Obsidian-style Markdown notes in the Knowledge Library.
// Beats, once in view:
// 1. Opens directly on a borderless Markdown canvas (Category picker + title + toolbar, zero form boxes).
// 2. Live-types an Obsidian callout (`> [!tip]`) and a task checklist item with live syntax styling.
// 3. Cursor glides to Preview and clicks -> switches to rendered Markdown with green callout,
//    syntax-highlighted Rust code fence, and interactive checklist.
// 4. Cursor clicks the unchecked task checkbox -> ticks smoothly to done. Holds on payoff.

const LINE_CALLOUT = '> [!tip] Keep writes atomic'
const LINE_TASK = '- [ ] Set busy_timeout on every reader'

type S = {
  mode: 'write' | 'preview'
  callout: string
  task: string
  checked: boolean
}

const initial: S = {
  mode: 'write',
  callout: '',
  task: '',
  checked: false,
}

async function script(a: Act<S>) {
  await a.wait(650)
  await a.type('callout', LINE_CALLOUT, { interval: 32 })
  await a.wait(220)
  await a.type('task', LINE_TASK, { interval: 28 })
  await a.wait(500)
  await a.click('nb-preview')
  a.mark('preview')
  await a.set({ mode: 'preview' })
  await a.wait(850)
  await a.click('nb-task')
  a.mark('checked')
  await a.set({ checked: true })
  await a.wait(1600)
}

const CATEGORIES = [
  ['Rust', '19', true],
  ['Design & UI', '13', false],
  ['Database', '9', false],
  ['Tooling', '7', false],
] as const

function Side() {
  return (
    <nav className="nd-side">
      <div className="nd-new-btn">+ New Note</div>
      <div className="nd-kind">
        <span>All notes</span>
        <span className="nd-count">48</span>
      </div>
      <div className="nd-side-label">Categories</div>
      {CATEGORIES.map(([name, n, open]) => (
        <div key={name} className="nd-cat">
          <div className="nd-kind">
            <span>{open ? '⌄ ' : '› '}{name}</span>
            <span className="nd-count">{n}</span>
          </div>
          {open && (
            <div className="nd-tree-item" data-on>
              SQLite WAL across pro…
            </div>
          )}
        </div>
      ))}
    </nav>
  )
}

function LiveCalloutLine({ text, showCaret }: { text: string; showCaret: boolean }) {
  if (!text) return null
  const prefix = '> [!tip]'
  if (text.startsWith(prefix)) {
    return (
      <div className="nd-md-line">
        <span className="nd-md-dim">{prefix}</span>
        <span className="nd-md-tip">{text.slice(prefix.length)}</span>
        {showCaret && <span className="nd-caret" aria-hidden="true" />}
      </div>
    )
  }
  return (
    <div className="nd-md-line">
      <span className="nd-md-dim">{text}</span>
      {showCaret && <span className="nd-caret" aria-hidden="true" />}
    </div>
  )
}

function LiveTaskLine({ text, showCaret }: { text: string; showCaret: boolean }) {
  if (!text) return null
  const prefix = '- [ ]'
  if (text.startsWith(prefix)) {
    return (
      <div className="nd-md-line">
        <span className="nd-md-dim">{prefix}</span>
        <span>{text.slice(prefix.length)}</span>
        {showCaret && <span className="nd-caret" aria-hidden="true" />}
      </div>
    )
  }
  return (
    <div className="nd-md-line">
      <span className="nd-md-dim">{text}</span>
      {showCaret && <span className="nd-caret" aria-hidden="true" />}
    </div>
  )
}

export default function NotebookDemo() {
  const theme = useSiteTheme()
  return (
    <Stage<S>
      width={960}
      height={520}
      compact={{ below: 720, width: 358, height: 480 }}
      theme={theme}
      greys="cool"
      play="once"
      initial={initial}
      poster={{ mode: 'preview', callout: LINE_CALLOUT, task: LINE_TASK, checked: true }}
      script={script}
      cursor={{ at: { wide: { x: 760, y: 420 }, compact: { x: 290, y: 380 } }, style: 'real' }}
      className="nd"
    >
      {(s, _rect, layout) => (
        <Window
          className="nd-win"
          title="Notebook"
          sidebar={layout === 'compact' ? undefined : <Side />}
          sidebarWidth={196}
          toolbar={layout === 'compact' ? undefined : <Segmented className="nd-nav-seg" items={['Home', 'Feed', 'Notebook']} value={2} />}
        >
          <div className="nd-canvas">
            <header className="nd-topbar">
              <div className="nd-cat-pick">
                <span className="nd-cat-pick__label">Category</span>
                <span className="nd-cat-pick__val">Rust ⌄</span>
              </div>
              <div className="nd-topbar__actions">
                <div className="nd-mode-seg">
                  <span className="nd-mode-seg__btn" data-on={s.mode === 'write' || undefined}>Write</span>
                  <span
                    className="nd-mode-seg__btn"
                    data-on={s.mode === 'preview' || undefined}
                    data-target="nb-preview"
                    data-cursor="pointer"
                  >
                    Preview
                  </span>
                </div>
                <Button variant="primary" className="nd-save-btn">Save Note</Button>
              </div>
            </header>

            <h3 className="nd-doc-title">SQLite WAL across processes</h3>

            {s.mode === 'write' ? (
              <div className="nd-editor" key="write">
                <div className="nd-fmtbar" aria-hidden="true">
                  <span>H1</span>
                  <span>H2</span>
                  <span>H3</span>
                  <i />
                  <b>B</b>
                  <em>I</em>
                  <span>&lt;&gt;</span>
                  <span>Link</span>
                  <i />
                  <span>• List</span>
                  <span>☐ Task</span>
                  <i />
                  <span>Callout</span>
                  <span>Code</span>
                </div>
                <div className="nd-md-body">
                  <div className="nd-md-line">
                    When both <span className="nd-md-code">`feeder-app`</span> and <span className="nd-md-code">`feeder-cli`</span> share <span className="nd-md-code">`feeder.db`</span>, keep read transactions short so WAL checkpoints never stall.
                  </div>
                  <div className="nd-md-line nd-md-h2">
                    <span className="nd-md-dim">## </span>Checklist
                  </div>
                  <div className="nd-md-line">
                    <span className="nd-md-dim">- [x] </span>Verify <span className="nd-md-code">`PRAGMA foreign_keys = ON`</span>
                    {!s.callout && !s.task && <span className="nd-caret" aria-hidden="true" />}
                  </div>
                  <LiveCalloutLine text={s.callout} showCaret={s.callout.length > 0 && s.callout !== LINE_CALLOUT} />
                  {s.callout === LINE_CALLOUT && (
                    <div className="nd-md-line">
                      <span className="nd-md-dim">&gt; </span>Copy media files before opening the transaction.
                      {!s.task && <span className="nd-caret" aria-hidden="true" />}
                    </div>
                  )}
                  <LiveTaskLine text={s.task} showCaret={s.task.length > 0} />
                </div>
              </div>
            ) : (
              <article className="nd-article" key="preview">
                <p>
                  When both <code>feeder-app</code> and <code>feeder-cli</code> share <code>feeder.db</code>, keep read
                  transactions short so WAL checkpoints never stall.
                </p>

                <div className="nd-callout">
                  <div className="nd-callout__title">Keep writes atomic</div>
                  <p>Copy media files before opening the transaction so disk and SQLite never drift.</p>
                </div>

                <pre className="nd-codeblock">
                  <code>
                    <span className="nd-syn-kw">let</span> tx = conn.<span className="nd-syn-fn">unchecked_transaction</span>()?;{'\n'}
                    conn.<span className="nd-syn-fn">busy_timeout</span>(Duration::<span className="nd-syn-fn">from_secs</span>(<span className="nd-syn-num">5</span>))?;
                  </code>
                </pre>

                <div className="nd-tasks">
                  <div className="nd-task" data-checked>
                    <span className="nd-box" data-checked>✓</span>
                    <span>Verify <code>PRAGMA foreign_keys = ON</code></span>
                  </div>
                  <div className="nd-task" data-checked={s.checked || undefined}>
                    <span
                      className="nd-box"
                      data-checked={s.checked || undefined}
                      data-target="nb-task"
                      data-cursor="pointer"
                    >
                      {s.checked ? '✓' : ''}
                    </span>
                    <span>Set <code>busy_timeout</code> on every reader</span>
                  </div>
                </div>
              </article>
            )}
          </div>
        </Window>
      )}
    </Stage>
  )
}
