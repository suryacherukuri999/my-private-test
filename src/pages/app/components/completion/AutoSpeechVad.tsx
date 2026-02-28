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
  startNewConversation: UseCompletionReturn["startNewConversation"];
  hasExistingChat: boolean;
  response: string;
}

const AutoSpeechVADInternal = ({
  submit,
  setState,
  setEnableVAD,
  microphoneDeviceId,
  startNewConversation,
  hasExistingChat,
  response,
}: AutoSpeechVADProps) => {
  const [isTranscribing, setIsTranscribing] = useState(false);
  const [listening, setListening] = useState(false);
  const [showChatChoice, setShowChatChoice] = useState(false);
  // Track whether we submitted and are waiting for the first response char
  const [waitingForResponse, setWaitingForResponse] = useState(false);
  const { selectedSttProvider, allSttProviders } = useApp();
  const unlistenRef = useRef<(() => void) | null>(null);

  // Stop mic capture helper
  const stopMic = useCallback(async () => {
    await invoke("stop_mic_capture").catch(() => {});
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }
    setListening(false);
    setEnableVAD(false);
  }, [setEnableVAD]);

  // Watch for first character of response — stop mic when it appears
  useEffect(() => {
    if (waitingForResponse && response.length > 0) {
      stopMic();
      setWaitingForResponse(false);
    }
  }, [waitingForResponse, response, stopMic]);

  // Handle speech detected from Rust backend
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
          // Mark that we're waiting for the first response character to stop mic
          setWaitingForResponse(true);
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

  // Start mic capture helper
  const startMic = useCallback(async () => {
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
      console.error("Failed to start mic capture:", error);
      setState((prev: any) => ({
        ...prev,
        error: `Mic capture failed: ${error}`,
      }));
    }
  }, [microphoneDeviceId, setEnableVAD, setState, handleSpeechDetected]);

  // Handle mic button click
  const handleMicClick = async () => {
    if (listening) {
      await stopMic();
      return;
    }

    if (hasExistingChat) {
      setShowChatChoice(true);
    } else {
      await startMic();
    }
  };

  const handleContinueCurrentChat = async () => {
    setShowChatChoice(false);
    await startMic();
  };

  const handleNewChat = async () => {
    setShowChatChoice(false);
    startNewConversation();
    await startMic();
  };

  if (showChatChoice) {
    return (
      <div className="flex flex-col gap-1">
        <Button
          size="sm"
          variant="outline"
          onClick={handleContinueCurrentChat}
          className="text-xs h-7 px-2"
        >
          Continue chat
        </Button>
        <Button
          size="sm"
          variant="outline"
          onClick={handleNewChat}
          className="text-xs h-7 px-2"
        >
          New chat
        </Button>
      </div>
    );
  }

  return (
    <>
      <Button
        size="icon"
        onClick={handleMicClick}
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
