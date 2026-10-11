//! The remote command line of one `ssh` provider call.
//!
//! Each call is one `bash -c` of the embedded script [`SLOT_SCRIPT`], preceded
//! by assignments of its inputs. Every value is single-quoted into the
//! script text, so nothing the user or a task supplied is ever parsed as shell
//! by the login shell of the host (which only sees `bash -c '<one quoted
//! word>'`).

use super::job::sh_quote;

/// The remote half of the provider, embedded in the binary.
pub const SLOT_SCRIPT: &str = include_str!("ssh-slot.sh");

/// The umask of a slot that only its login user may enter.
pub const PRIVATE_UMASK: &str = "077";

/// How many argument variables the script has (`A1`..`A4`); all are always
/// assigned, so none can be inherited from the host's environment.
const ARGS: usize = 4;

/// The command line for `verb` with arguments `args` (`A1`, `A2`, ...), for
/// the slot root `root`. `login` runs the script under a login shell, so
/// what it reads of the environment is what a login of that user has.
#[must_use]
pub fn command_line(root: &str, verb: &str, args: &[&str], login: bool) -> String {
    command_line_as(root, verb, args, login, PRIVATE_UMASK)
}

/// Like [`command_line`], with the umask the script writes under.
#[must_use]
pub fn command_line_as(root: &str, verb: &str, args: &[&str], login: bool, umask: &str) -> String {
    let mut script = format!(
        "ROOT={}\nVERB={}\nUMASK={}\n",
        sh_quote(root),
        sh_quote(verb),
        sh_quote(umask)
    );
    for i in 0..ARGS {
        let arg = args.get(i).copied().unwrap_or_default();
        script.push_str(&["A", &(i + 1).to_string(), "=", &sh_quote(arg), "\n"].concat());
    }
    script.push_str(SLOT_SCRIPT);
    let shell = if login { "bash -l -c" } else { "bash -c" };
    format!("{shell} {}", sh_quote(&script))
}
