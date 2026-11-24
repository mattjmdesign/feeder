import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Bar, Window } from '../stageshow/kit/window'
import { Button } from '../stageshow/kit/controls/Button'
import { Segmented } from '../stageshow/kit/controls/Segmented'
import { Collapse, Stream } from '../stageshow/kit/motion'
import { Check } from '../stageshow/kit/parts/Check'
import { useSiteTheme } from './theme'
import './curator-demo.css'

const ANSWER =
  'Atlas API now counts rate limits per API key instead of per IP. Nothing in the Library covered it, so I proposed a note your agents will find next time.'
const WORDS = ANSWER.split(' ').length

type Step = 'idle' | 'running' | 'done'
type S = {
  flagged: boolean
  read: Step
  search: Step
  words: number
  proposal: boolean
  applied: boolean
}

const initial: S = { flagged: true, read: 'idle', search: 'idle', words: 0, proposal: false, applied: false }
const poster: S = { flagged: false, read: 'done', search: 'done', words: WORDS, proposal: true, applied: true }

async function script(a: Act<S>) {
  await a.wait(900)
  await a.click('review-now')
  a.mark('review')
  a.put({ flagged: false, read: 'running' })
  await a.wait(1000)
  a.put({ read: 'done', search: 'running' })
  await a.wait(950)
  a.put({ search: 'done' })
  await a.wait(300)
  a.mark('answer')
  for (let i = 1; i <= WORDS; i++) {
    a.put({ words: i })
    await a.wait(38)
  }
  await a.wait(350)
  a.mark('proposal')
  a.put({ proposal: true })
  await a.wait(700)
  await a.click('apply-note')
  a.mark('applied')
  a.put({ applied: true })
  await a.wait(2600)
}

function Side() {
  return (
    <nav className="cd2-side">
      <div className="cd2-new">+ New Chat</div>
      <div className="cd2-label">Chats</div>
      <div className="cd2-day">Today</div>
      <div className="cd2-thread" data-on>
        Review flagged updates
      </div>
      <div className="cd2-thread">How do we ship the Mac app?</div>
    </nav>
  )
}

function CuratorPill() {
  return (
    <span className="cd2-pill" data-on>
      <span className="cd2-orb" aria-hidden="true" />
      Curator
    </span>
  )
}

function StepRow({ state, label }: { state: Step; label: string }) {
  return (
    <Collapse open={state !== 'idle'}>
      <div className="cd2-step">
        <Check on={state === 'done'} pending={state === 'running'} size={13} />
        <span>{label}</span>
      </div>
    </Collapse>
  )
}

export default function CuratorDemo() {
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
      poster={poster}
      script={script}
      cursor={{ at: { wide: { x: 820, y: 470 }, compact: { x: 300, y: 380 } }, style: 'real' }}
      className="cd2"
    >
      {(s, _rect, layout) => (
        <Window
          className="cd2-win"
          title="Curator"
          sidebar={layout === 'compact' ? undefined : <Side />}
          sidebarWidth={196}
          toolbar={
            layout === 'compact' ? undefined : (
              <span className="cd2-tools">
                <Segmented className="cd2-seg" items={['Home', 'Feed', 'Library']} value={-1} />
                <CuratorPill />
              </span>
            )
          }
        >
          <div className="cd2-main">
            <div className="cd2-thread-col">
              <div className="cd2-past" aria-hidden="true">
                <Bar w={190} h={28} r={14} tone={1} className="cd2-past-q" />
                <Bar w="78%" h={8} r={4} tone={2} />
                <Bar w="56%" h={8} r={4} tone={2} />
              </div>
              <StepRow state={s.read} label="Read flagged update · Atlas API" />
              <StepRow state={s.search} label="Searched notes for “rate limit”" />
              <Collapse open={s.words > 0}>
                <p className="cd2-answer">
                  <Stream text={ANSWER} count={s.words} />
                </p>
              </Collapse>
              <Collapse open={s.proposal} reveal="drop" from="below">
                <div className="cd2-note">
                  <div className="cd2-note__text">
                    <span className="cd2-note__kind">{s.applied ? 'Added to Library' : 'Proposed new note'}</span>
                    <span className="cd2-note__title">Rate limits count per API key</span>
                    <span className="cd2-note__src">From Atlas API · Claude Code</span>
                  </div>
                  <div className="cd2-note__actions">
                    {s.applied ? (
                      <Button size="small" className="cd2-revert">Revert</Button>
                    ) : (
                      <>
                        <Button size="small" className="cd2-revert">Dismiss</Button>
                        <Button variant="primary" size="small" className="cd2-revert" data-target="apply-note">Apply</Button>
                      </>
                    )}
                  </div>
                </div>
              </Collapse>
            </div>
            <div className="cd2-foot">
              <Collapse open={s.flagged}>
                <div className="cd2-flag">
                  <span className="cd2-flag__mark" aria-hidden="true">
                    <span className="cd2-flag__dot" />
                  </span>
                  <span>1 flagged update to review</span>
                  <span className="cd2-flag__btn" data-target="review-now" data-cursor="pointer">
                    Review Now
                  </span>
                </div>
              </Collapse>
              <div className="cd2-composer">Ask Curator…</div>
            </div>
          </div>
        </Window>
      )}
    </Stage>
  )
}
