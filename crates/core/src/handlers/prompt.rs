//! `prompt.*`: ответы на inline-запросы.

use super::{arg, meta};
use crate::bus::Handler;
use crate::cmd::{ArgSpec, CmdError, Command, CommandMeta, ValueKind};
use crate::ids::PromptId;
use crate::msg::Cx;
use crate::prompt::Reply;
use crate::state::{Focus, UiEvent};

pub(crate) struct PromptHandler;

const ID: ArgSpec = arg("id", ValueKind::Int, true);

static COMMANDS: [CommandMeta; 2] = [
    meta(
        "prompt.reply",
        "Answer",
        &[
            ID,
            arg("choice", ValueKind::Str, true),
            arg("input", ValueKind::Str, false),
            arg("all", ValueKind::Bool, false),
        ],
        false,
    ),
    meta("prompt.cancel", "Dismiss", &[ID], false),
];

/// `choice` ответа на вето выхода, при котором приложение всё-таки выходит
/// (задачи отменяются) — по умолчанию `Handler::on_quit_reply`. Отмена — остаёмся.
pub(crate) const QUIT_CHOICE: &str = "quit";

impl Handler for PromptHandler {
    fn namespace(&self) -> &'static str {
        "prompt"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let a = &cmd.args;
        let id = a
            .int("id")
            .and_then(|v| u64::try_from(v).ok())
            .map(PromptId)
            .ok_or_else(|| CmdError::arg("id", "expected a prompt id"))?;
        let Some(job) = cx.state.prompts.get(id).map(|p| p.job) else {
            return Err(CmdError::arg("id", "no such prompt"));
        };
        // Вопрос выхода: ответ решает handler, наложивший вето (core передаёт его
        // после этой команды, `Handler::on_quit_reply`).
        let quit = cx
            .state
            .quit_prompt
            .filter(|(p, _)| *p == id)
            .map(|(_, h)| h);
        if cmd.id.as_str() == "prompt.reply" {
            let choice = a.str("choice").unwrap_or_default();
            cx.state.prompts.reply(
                id,
                Reply {
                    choice: choice.into(),
                    input: a.str("input").map(str::to_owned),
                    all: a.bool("all").unwrap_or(false),
                },
            );
            if let Some(j) = job {
                cx.state.jobs.answered(j, id);
            }
            if let Some(h) = quit {
                cx.state.quit_reply = Some((h, choice.into()));
            }
        } else {
            // Отказ отвечать на вопрос задачи отменяет задачу. Сначала задачу:
            // закрытие запроса будит `ask`, и воркер должен увидеть уже отменённый
            // токен, иначе Drop ручки даст `Failed("aborted")`.
            if let Some(j) = job {
                cx.state.jobs.cancel(j);
            }
            cx.state.prompts.cancel(id);
        }
        if quit.is_some() {
            cx.state.quit_prompt = None;
        }
        if cx.state.focus == Focus::Prompt(id) {
            cx.state.focus = Focus::Panel;
            cx.ui(UiEvent::FocusChanged(Focus::Panel));
        }
        Ok(())
    }
}
