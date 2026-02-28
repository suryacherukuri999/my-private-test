import { fetchSTT } from "@/lib";
import { UseCompletionReturn } from "@/types";
import { LoaderCircleIcon, MicIcon, MicOffIcon } from "lucide-react";
import { useState, useEffect, useRef, useCallback } from "react";
import { Button } from "@/components";
import { useApp } from "@/contexts";
import { shouldUsePluelyAPI } from "@/lib/functions/pluely.api";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface AutoSpeechVADProps {
  submit: UseCompletionReturn["submit"];
  setState: UseCompletionReturn["setState"];
  setEnableVAD: UseCompletionReturn["setEnableVAD"];
  microphoneDeviceId: string;
}

const AutoSpeechVADInternal = ({
  submit,
  setState,
  setEnableVAD,
  microphoneDeviceId,
}: AutoSpeechVADProps) => {
  const [isTranscribing, setIsTranscribing] = useState(false);
  const [listening, setListening] = useState(false);
  const { selectedSttProvider, allSttProviders } = useApp();
  const unlistenRef = useRef<(() => void) | null>(null);

  // Handle speech detected from Rust backend (native mic capture via cpal)
  const handleSpeechDetected = useCallback(
    async (base64Audio: string) => {
      try {
        const binaryString = atob(base64Audio);
        const bytes = new Uint8Array(binaryString.length);
        for (let i = 0; i < binaryString.length; i++) {
          bytes[i] = binaryString.charCodeAt(i);
        }
        const audioBlob = new Blob([bytes], { type: "audio/wav" });

        const usePluelyAPI = await shouldUsePluelyAPI();

        if (!selectedSttProvider.provider && !usePluelyAPI) {
          setState((prev: any) => ({
            ...prev,
            error:
              "No speech provider selected. Please select one in settings.",
          }));
          return;
        }

        const providerConfig = allSttProviders.find(
          (p) => p.id === selectedSttProvider.provider
        );

        if (!providerConfig && !usePluelyAPI) {
          setState((prev: any) => ({
            ...prev,
            error:
              "Speech provider configuration not found. Please check your settings.",
          }));
          return;
        }

        setIsTranscribing(true);

        const transcription = await fetchSTT({
          provider: usePluelyAPI ? undefined : providerConfig,
          selectedProvider: selectedSttProvider,
          audio: audioBlob,
        });

        if (transcription) {
          submit(transcription);
        }
      } catch (error) {
        console.error("Failed to transcribe audio:", error);
        setState((prev: any) => ({
          ...prev,
          error:
            error instanceof Error ? error.message : "Transcription failed",
        }));
      } finally {
        setIsTranscribing(false);
      }
    },
    [selectedSttProvider, allSttProviders, submit, setState]
  );

  // Start native mic capture on mount (uses cpal in Rust — no browser getUserMedia)
  useEffect(() => {
    let cancelled = false;

    const startCapture = async () => {
      try {
        const deviceName =
          microphoneDeviceId && microphoneDeviceId !== "default"
            ? microphoneDeviceId
            : null;

        await invoke("start_mic_capture", { deviceName });

        if (!cancelled) {
          setListening(true);
          setEnableVAD(true);
        }

        const unlisten = await listen<string>(
          "mic-speech-detected",
          (event) => {
            if (!cancelled) {
              handleSpeechDetected(event.payload);
            }
          }
        );

        unlistenRef.current = unlisten;
      } catch (error) {
        console.error("Failed to start native mic capture:", error);
        if (!cancelled) {
          setState((prev: any) => ({
            ...prev,
            error: `Mic capture failed: ${error}`,
          }));
        }
      }
    };

    startCapture();

    return () => {
      cancelled = true;
      invoke("stop_mic_capture").catch(() => {});
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
      setListening(false);
    };
  }, [microphoneDeviceId]); // eslint-disable-line react-hooks/exhaustive-deps

  const toggleListening = async () => {
    if (listening) {
      await invoke("stop_mic_capture").catch(() => {});
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
      setListening(false);
      setEnableVAD(false);
    } else {
      try {
        const deviceName =
          microphoneDeviceId && microphoneDeviceId !== "default"
            ? microphoneDeviceId
            : null;

        await invoke("start_mic_capture", { deviceName });
        setListening(true);
        setEnableVAD(true);

        const unlisten = await listen<string>(
          "mic-speech-detected",
          (event) => {
            handleSpeechDetected(event.payload);
          }
        );
        unlistenRef.current = unlisten;
      } catch (error) {
        console.error("Failed to resume mic capture:", error);
      }
    }
  };

  return (
    <>
      <Button
        size="icon"
        onClick={toggleListening}
        className="cursor-pointer"
      >
        {isTranscribing ? (
          <LoaderCircleIcon className="h-4 w-4 animate-spin text-green-500" />
        ) : listening ? (
          <MicOffIcon className="h-4 w-4 animate-pulse" />
        ) : (
          <MicIcon className="h-4 w-4" />
        )}
      </Button>
    </>
  );
};

export const AutoSpeechVAD = (props: AutoSpeechVADProps) => {
  return <AutoSpeechVADInternal key={props.microphoneDeviceId} {...props} />;
};
