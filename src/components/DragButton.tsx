import { GripVerticalIcon } from "lucide-react";
import { Button } from "@/components";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

export const DragButton = () => {
  const handleMouseDown = async (e: React.MouseEvent) => {
    e.preventDefault();
    try {
      await getCurrentWebviewWindow().startDragging();
    } catch (err) {
      console.error("Failed to start dragging:", err);
    }
  };

  return (
    <Button
      variant="ghost"
      size="icon"
      className={`-ml-[2px] w-fit cursor-grab`}
      onMouseDown={handleMouseDown}
    >
      <GripVerticalIcon className="h-4 w-4" />
    </Button>
  );
};
