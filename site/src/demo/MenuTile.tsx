import { Stage } from '../stageshow/kit/stage'
import { useSiteTheme } from './theme'
import './demos.css'

// The menu bar companion, opened. A still: the marble icon pressed in the
// menu bar and its menu below it, one project waiting on you, one working,
// the rest recent. Mirrors crates/feeder-menubar.
//
// Real strings: section names, project names and their notes.

function Marbles({ attention }: { attention?: boolean }) {
  return (
    <svg viewBox="0 0 18 18" width="16" height="16" aria-hidden="true">
      <g fill="currentColor" transform="translate(0 18) scale(1 -1)">
        <circle cx="6" cy="7" r="5" />
        <circle cx="14.5" cy="4.5" r="2.5" />
        <circle cx="7.25" cy="14.75" r="2.25" />
        {attention && <circle cx="15" cy="14" r="3" />}
      </g>
    </svg>
  )
}

const ROWS = [
  { section: 'Waiting on you', name: 'Lumen', note: 'Cursor', dot: 'var(--ss-warning)' },
  { section: 'Working now', name: 'Orbit', note: 'Writing tests · Codex', dot: 'var(--ss-success)' },
  { section: 'Recent', name: 'Atlas API', note: 'Claude Code', dot: 'var(--ss-accent)' },
  { name: 'Ledger', note: 'T3 Code', dot: 'transparent' },
]

export default function MenuTile() {
  const theme = useSiteTheme()
  return (
    <Stage width={320} height={260} fill theme={theme} greys="cool" initial={{}} className="mt">
      {() => (
        <>
          <div className="mt-bar">
            <span className="mt-item" data-on>
              <Marbles attention />
            </span>
            <span className="mt-clock">9:41</span>
          </div>
          <div className="mt-menu" role="menu">
            {ROWS.map(r => (
              <div key={r.name}>
                {r.section && <div className="mt-section">{r.section}</div>}
                <div className="mt-row">
                  <i style={{ background: r.dot }} />
                  <span className="mt-name">{r.name}</span>
                  <span className="mt-note">{r.note}</span>
                </div>
              </div>
            ))}
          </div>
        </>
      )}
    </Stage>
  )
}
