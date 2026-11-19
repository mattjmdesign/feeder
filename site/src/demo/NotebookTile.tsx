import { Stage } from '../stageshow/kit/stage'
import { useSiteTheme } from './theme'
import './demos.css'

// One notebook page, the thing an agent files after a hard fix. A still: the
// kind, the title, the rule to follow, and how often it has helped.
//
// Real strings: all of them; the page is the subject.

export default function NotebookTile() {
  const theme = useSiteTheme()
  return (
    <Stage width={320} height={240} fill theme={theme} greys="cool" initial={{}} className="nt">
      {() => (
        <article className="nt-page">
          <div className="nt-kind">
            <span className="nt-chip">Fix</span>
            <span className="nt-tags">sqlite · rust</span>
          </div>
          <div className="nt-title">Writes fail with SQLITE_BUSY</div>
          <div className="nt-label">Do this</div>
          <p className="nt-do">Set a busy timeout when you open each connection.</p>
          <div className="nt-foot">Helped in 3 projects</div>
        </article>
      )}
    </Stage>
  )
}
