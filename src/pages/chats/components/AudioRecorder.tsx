import { useState, useRef, useEffect } from "react";
import { Button } from "@/components";
import { AudioVisualizer } from "@/pages/app/components/speech/audio-visualizer";
import { shouldUsePluelyAPI, fetchSTT } from "@/lib";
import { useApp } from "@/contexts";
import { StopCircle, Send } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface AudioRecorderProps {
  onTranscriptionComplete: (text: string) => void;
  onCancel: () => void;
}

const MAX_DURATION = 3 * 60 * 1000;

export const AudioRecorder = ({
  onTranscriptionComplete,
  onCancel,
}: AudioRecorderProps) => {
  const { selectedSttProvider, allSttProviders } = useApp();
  const [isTranscribing, setIsTranscribing] = useState(false);
  const [duration, setDuration] = useState(0);
  const [isRecording, setIsRecording] = useState(false);

  const audioChunksRef = useRef<string[]>([]);
  const startTimeRef = useRef<number>(0);
  const durationIntervalRef = useRef<NodeJS.Timeout | null>(null);
  const maxDurationTimeoutRef = useRef<NodeJS.Timeout | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    startRecording();
    return () => cleanup();
  }, []);

  const cleanup = () => {
    if (durationIntervalRef.current) {
      clearInterval(durationIntervalRef.current);
      durationIntervalRef.current = null;
    }
    if (maxDurationTimeoutRef.current) {
      clearTimeout(maxDurationTimeoutRef.current);
      maxDurationTimeoutRef.current = null;
    }
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }
    // Stop native mic capture
    invoke("stop_mic_capture").catch(() => {});
    setIsRecording(false);
  };

  const startRecording = async () => {
    try {
      // Start native mic capture via Rust backend (no browser getUserMedia)
      await invoke("start_mic_capture", { deviceName: null });
      setIsRecording(true);

      audioChunksRef.current = [];
      startTimeRef.current = Date.now();

      // Listen for speech segments from Rust backend
      const unlisten = await listen<string>(
        "mic-speech-detected",
        (event) => {
          audioChunksRef.current.push(event.payload);
        }
      );
      unlistenRef.current = unlisten;

      durationIntervalRef.current = setInterval(() => {
        setDuration(Date.now() - startTimeRef.current);
      }, 100);

      maxDurationTimeoutRef.current = setTimeout(() => {
        handleSend();
      }, MAX_DURATION);
    } catch (error) {
      console.error("Failed to start recording:", error);
      cleanup();
      onCancel();
    }
  };

  const handleStop = () => {
    cleanup();
    onCancel();
  };

  const handleSend = async () => {
    if (isTranscribing) return;

    setIsTranscribing(true);

    // Get the last speech segment (most recent complete utterance)
    const lastChunk = audioChunksRef.current[audioChunksRef.current.length - 1];

    cleanup();

    if (!lastChunk) {
      // No speech detected — cancel
      onCancel();
      return;
    }

    try {
      // Convert base64 WAV to blob
      const binaryString = atob(lastChunk);
      const bytes = new Uint8Array(binaryString.length);
      for (let i = 0; i < binaryString.length; i++) {
        bytes[i] = binaryString.charCodeAt(i);
      }
      const audioBlob = new Blob([bytes], { type: "audio/wav" });

      const usePluelyAPI = await shouldUsePluelyAPI();
      const provider = allSttProviders.find(
        (p) => p.id === selectedSttProvider.provider
      );

      const text = await fetchSTT({
        provider: usePluelyAPI ? undefined : provider,
        selectedProvider: selectedSttProvider,
        audio: audioBlob,
      });

      onTranscriptionComplete(text);
    } catch (error) {
      console.error("Transcription failed:", error);
      onCancel();
    }
  };

  const formatTime = (ms: number) => {
    const seconds = Math.floor(ms / 1000);
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}:${secs.toString().padStart(2, "0")}`;
  };

  return (
    <div className="border bg-background rounded-lg overflow-hidden">
      <div className="h-12 relative bg-muted/20">
        {isRecording ? (
          <div className="h-full w-full pt-3">
            <AudioVisualizer stream={null} isRecording={true} />
          </div>
        ) : (
          <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
            Initializing...
          </div>
        )}
      </div>
      <div className="flex items-center justify-between px-4 py-2.5 border-t bg-muted/5">
        <div className="flex items-center gap-2">
          <div className="h-2 w-2 bg-red-500 rounded-full animate-pulse" />
          <span className="text-sm font-mono tabular-nums font-medium">
            {formatTime(duration)}
          </span>
          <span className="text-xs text-muted-foreground">/ 3:00</span>
        </div>
        <div className="flex items-center gap-2">
          <Button
            size="icon"
            variant="outline"
            onClick={handleStop}
            disabled={isTranscribing}
            className="h-8 w-8"
            title="Stop recording"
          >
            <StopCircle className="h-4 w-4" />
          </Button>
          <Button
            size="icon"
            onClick={handleSend}
            disabled={isTranscribing}
            className="h-8 w-8"
            title={isTranscribing ? "Sending..." : "Send to AI"}
          >
            <Send className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  );
};
