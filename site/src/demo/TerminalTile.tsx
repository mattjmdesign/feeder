import { Stage } from '../stageshow/kit/stage'
import { Caret, Prompt, Window } from '../stageshow/kit/window'
import { useSiteTheme } from './theme'
import './demos.css'

// A new agent session reading where the last one stopped. A still: the
// terminal opens on the feed command and its last update, Next included.
//
// Real strings: the command and the update it prints.

export default function TerminalTile() {
  const theme = useSiteTheme()
  return (
    <Stage width={320} height={240} fill theme={theme} greys="cool" initial={{}} className="tt">
      {() => (
        <Window chrome="terminal" title="harbor — zsh" className="tt-win">
          <Prompt path="~/harbor">feeder feed -n 1</Prompt>
          <div className="dim">Harbor · Claude Code · yesterday</div>
          <div>Done: upload queue</div>
          <div className="ok">Next: retry failed uploads</div>
          <Prompt path="~/harbor">
            <Caret />
          </Prompt>
        </Window>
      )}
    </Stage>
  )
}
