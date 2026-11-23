import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Prompt, Window } from '../stageshow/kit/window'
import { ORBS, orbBackground } from '../lib/orb'
import { ToolLogo } from './ToolLogo'
import { useSiteTheme } from './theme'
import './demos.css'

type S = { step: number }

const initial: S = { step: 0 }

async function script(a: Act<S>) {
  const beats = [700, 900, 1100, 1300, 900, 700]
  for (const [i, ms] of beats.entries()) {
    await a.wait(ms)
    a.put({ step: i + 1 })
  }
  a.mark('posted')
  await a.wait(1200)
}

function Line({ on, children, className }: { on: boolean; children: React.ReactNode; className?: string }) {
  return (
    <div className={`md-line${className ? ` ${className}` : ''}`} data-on={on || undefined}>
      {children}
    </div>
  )
}

export default function McpDemo() {
  const theme = useSiteTheme()
  return (
    <Stage<S>
      width={960}
      height={420}
      compact={{ below: 720, width: 358, height: 470 }}
      theme={theme}
      greys="cool"
      play="once"
      initial={initial}
      poster={{ step: 6 }}
      script={script}
      className="md"
    >
      {s => (
        <>
          <Window chrome="terminal" title="harbor — claude" className="md-term">
            <Prompt path="~/harbor">claude</Prompt>
            <Line on={s.step >= 1} className="md-ask">
              › Pick up where we left off
            </Line>
            <Line on={s.step >= 2} className="md-call">
              <span className="md-bullet">⏺</span> feeder · <b>get_feed</b>
            </Line>
            <Line on={s.step >= 3} className="md-result">
              ⎿ Last: uploads moved to a queue. You replied: retry failures first.
            </Line>
            <Line on={s.step >= 4} className="md-work">
              <span className="md-bullet">⏺</span> Adding retries with backoff
            </Line>
            <Line on={s.step >= 5} className="md-call">
              <span className="md-bullet">⏺</span> feeder · <b>post_update</b>
            </Line>
            <Line on={s.step >= 6} className="md-result">
              ⎿ Posted to Harbor
            </Line>
          </Window>

          <article className="md-card" data-on={s.step >= 6 || undefined}>
            <header className="md-card__head">
              <span className="demo-orb" style={{ width: 20, height: 20, background: orbBackground(ORBS.harbor!) }} />
              <span className="md-card__project">Harbor</span>
              <span className="md-card__agent">
                <ToolLogo tool="claude" size={12} />
                Claude Code
              </span>
              <span className="md-card__when">now</span>
            </header>
            <div className="md-label">Done</div>
            <p>Failed uploads retry three times with backoff.</p>
            <div className="md-label">Next</div>
            <p>Show the retry count in the upload list.</p>
          </article>
        </>
      )}
    </Stage>
  )
}
