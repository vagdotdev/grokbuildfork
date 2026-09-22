/// Events emitted by [`crate::pipeline::run_voice_pipeline`] to the pager event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    /// Partial transcript while the user is speaking (`interim_results` / non-final chunks).
    InterimTranscript { text: String },

    /// Utterance complete (`speech_final` on streaming STT, or batch result).
    UtteranceFinal { text: String },

    /// Workshop overlay: one-line progress for the recording banner while the local engine is
    /// getting ready ("Downloading voice model… 42%"). Empty text restores the plain banner.
    /// Never inserted into the prompt.
    Status { text: String },

    /// Non-fatal or fatal error from capture or STT.
    Error {
        /// Short description for a one-line toast.
        message: String,
        /// Optional longer fix steps, shown where more than one line fits.
        hint: Option<String>,
    },
}
