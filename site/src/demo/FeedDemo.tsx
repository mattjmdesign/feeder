import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Bar, Window } from '../stageshow/kit/window'
import { Collapse } from '../stageshow/kit/motion/Collapse'
import { Badge } from '../stageshow/kit/controls/Badge'
import { Segmented } from '../stageshow/kit/controls/Segmented'
import { Spinner } from '../stageshow/kit/parts/Spinner'
import { ORBS, orbBackground, type Orb } from '../lib/orb'
import { ToolLogo } from './ToolLogo'
import { useSiteTheme } from './theme'
import './feed-demo.css'

// Feeder's Feed while an agent works. Beats: a Working pulse opens at the top
// of the feed with a spinner; the agent posts, so the pulse closes as its
// update drops in and pushes the older card down; the update's last block is
// a decision, so its Needs you tag is part of the card from the start. The
// result holds, then the new card closes the way it opened and the loop
// starts again from the same feed.

type S = { working: boolean; posted: boolean }

const initial: S = { working: false, posted: false }

async function script(a: Act<S>) {
  await a.wait(800)
  a.mark('working')
  a.put({ working: true })
  await a.wait(2200)
  a.mark('posted')
  a.put({ working: false, posted: true })
  await a.wait(4200)
  a.put({ posted: false })
  await a.wait(900)
}

function OrbDot({ orb, size = 22 }: { orb: Orb; size?: number }) {
  return <span className="fd-orb" style={{ width: size, height: size, background: orbBackground(orb) }} />
}

export default function FeedDemo() {
  const theme = useSiteTheme()

  return (
    <Stage<S>
      width={960}
      height={540}
      compact={{ below: 720, width: 358, height: 340 }}
      theme={theme}
      greys="cool"
      initial={initial}
      poster={{ working: false, posted: true }}
      script={script}
      className="fd"
    >
      {(s, _rect, layout) => (
        <Window
          className="fd-win"
          toolbar={layout === 'compact' ? undefined : <Segmented className="fd-seg" items={['Home', 'Feed', 'Notebook']} value={1} />}
        >
          <div className="fd-body">
            <div className="fd-filters">
              <span className="fd-search">
                <Bar w={96} h={6} tone={2} />
              </span>
              <span className="fd-chip" data-on>All</span>
              <span className="fd-chip"><Bar w={54} h={6} tone={2} /></span>
              <span className="fd-chip"><Bar w={40} h={6} tone={2} /></span>
            </div>
            <div className="fd-day">
              <span>Today</span>
            </div>

            <div className="fd-list">
              <Collapse open={s.working}>
                <div className="fd-pulse">
                  <OrbDot orb={ORBS.orbit!} size={18} />
                  <span className="fd-pulse__name">Orbit</span>
                  <span className="fd-pulse__what">
                    <Spinner size={10} />
                    Working on onboarding
                  </span>
                  <span className="fd-card__spacer" />
                  <span className="fd-card__when">now</span>
                </div>
              </Collapse>

              <Collapse open={s.posted} reveal="drop">
                <article className="fd-card" data-lead>
                  <header className="fd-card__head">
                    <OrbDot orb={ORBS.orbit!} />
                    <span className="fd-card__project">Orbit</span>
                    <span className="fd-card__agent"><ToolLogo tool="codex" size={12} />Codex</span>
                    <span className="fd-card__spacer" />
                    <Badge tone="warning" dot className="fd-needs">Needs you</Badge>
                    <span className="fd-card__when">just now</span>
                  </header>
                  <div className="fd-block">
                    <div className="fd-label">Shipped</div>
                    <p>The new onboarding flow: three screens instead of seven, with progress saved between launches.</p>
                  </div>
                  <div className="fd-block" data-why>
                    <div className="fd-label">Why</div>
                    <p>Most people who quit did it on screen four of the old flow.</p>
                  </div>
                  <div className="fd-block">
                    <div className="fd-label">Decision needed</div>
                    <p>Keep the skip button on screen one, or drop it?</p>
                  </div>
                </article>
              </Collapse>

              <article className="fd-card">
                <header className="fd-card__head">
                  <OrbDot orb={ORBS.atlas!} />
                  <span className="fd-card__project">Atlas API</span>
                  <span className="fd-card__agent"><ToolLogo tool="claude" size={12} />Claude Code</span>
                  <span className="fd-card__spacer" />
                  <span className="fd-card__when">2h ago</span>
                </header>
                <div className="fd-block">
                  <div className="fd-label">Fixed</div>
                  <span className="fd-lines">
                    <Bar w="92%" h={7} r={3.5} tone={2} />
                    <Bar w="64%" h={7} r={3.5} tone={2} />
                  </span>
                </div>
                <div className="fd-block">
                  <Bar w={34} h={6} tone={1} />
                  <span className="fd-lines">
                    <Bar w="78%" h={7} r={3.5} tone={2} />
                  </span>
                </div>
              </article>
            </div>
          </div>
        </Window>
      )}
    </Stage>
  )
}
