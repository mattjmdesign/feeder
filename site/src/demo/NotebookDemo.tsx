import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Bar, Window } from '../stageshow/kit/window'
import { Field } from '../stageshow/kit/controls/Field'
import { useSiteTheme } from './theme'
import './demos.css'

// The notebook as a library across projects. Beats, once in view: an error
// is typed into the notebook's search; the list narrows to the one page that
// matches, written while working on a different project; the page opens with
// what happened and the rule to follow. The page holds.
//
// Real strings: the kinds in the sidebar, the query, the matching page and
// where it came from. Other pages in the list are placeholder.

const QUERY = 'SQLITE_BUSY'

type S = { query: string; found: boolean }

const initial: S = { query: '', found: false }

async function script(a: Act<S>) {
  await a.wait(800)
  await a.type('query', QUERY, { interval: 70 })
  await a.wait(350)
  a.mark('found')
  a.put({ found: true })
  await a.wait(1200)
}

const CATEGORIES = [
  ['All notes', '48'],
  ['Rust', '19'],
  ['Design & UI', '13'],
  ['Database', '9'],
  ['Tooling', '7'],
] as const

function Side() {
  return (
    <nav className="nd-side">
      {CATEGORIES.map(([name, n], i) => (
        <span key={name} className="nd-kind" data-on={i === 0 || undefined}>
          <span>{name}</span>
          <span className="nd-count">{n}</span>
        </span>
      ))}
    </nav>
  )
}

export default function NotebookDemo() {
  const theme = useSiteTheme()
  return (
    <Stage<S>
      width={960}
      height={440}
      compact={{ below: 720, width: 358, height: 440 }}
      theme={theme}
      greys="cool"
      play="once"
      initial={initial}
      poster={{ query: QUERY, found: true }}
      script={script}
      className="nd"
    >
      {(s, _rect, layout) => (
        <Window className="nd-win" title="Notebook" sidebar={layout === 'compact' ? undefined : <Side />} sidebarWidth={176}>
          <div className="nd-body">
            <div className="nd-list">
              <Field className="nd-search" value={s.query} placeholder="Search notebook" focused={!s.found} />
              <div className="nd-rows">
                <div className="nd-row nd-row--match" data-on={s.found || undefined}>
                  <span className="nd-row__title">Writes fail with SQLITE_BUSY</span>
                  <span className="nd-row__meta">
                    <span className="nd-tag">Rust</span> From Atlas API
                  </span>
                </div>
                {[180, 140, 200, 120].map((w, i) => (
                  <div key={i} className="nd-row nd-row--filler" data-gone={s.found || undefined}>
                    <Bar w={w} h={7} tone={2} />
                    <Bar w={84} h={6} tone={1} />
                  </div>
                ))}
              </div>
            </div>

            <article className="nd-page" data-on={s.found || undefined}>
              <div className="nd-page__kind">
                <span className="nd-tag">Rust</span>
                <span className="nd-page__from">Atlas API · Claude Code</span>
              </div>
              <div className="nd-page__title">Writes fail with SQLITE_BUSY</div>
              <p>
                The app froze while the CLI posted an update because the write held a lock while copying media files.
              </p>
              <div className="nd-callout">
                <div className="nd-callout__title">Keep writes atomic</div>
                <p>
                  Copy files before opening the transaction and set <code>busy_timeout</code> on every connection.
                </p>
              </div>
              <div className="nd-page__foot">Helped 3 times · last in Harbor</div>
            </article>
          </div>
        </Window>
      )}
    </Stage>
  )
}
