/* Parent runner sends TERM after its client has returned/reaped.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
static int await_client_completion(volatile sig_atomic_t *stop_flag,
                                   volatile sig_atomic_t *elapsed_ticks)
{
    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked,SIGTERM);
    sigaddset(&blocked,SIGINT);
    sigaddset(&blocked,SIGALRM);
    if (sigprocmask(SIG_BLOCK,&blocked,&previous)) return -1;
    /* Atomically unblock while waiting, avoiding signal-before-pause races.
     * No RECEIVE or response-buffer mutation while client consumes reply. */
    while (!*stop_flag && *elapsed_ticks<120) sigsuspend(&previous);
    int timed_out=!*stop_flag;
    if (sigprocmask(SIG_SETMASK,&previous,NULL)) return -1;
    return timed_out ? 1 : 0;
}
