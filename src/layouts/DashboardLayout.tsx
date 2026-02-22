import { Sidebar } from "@/components";
import { Outlet } from "react-router-dom";
import { ErrorBoundary } from "react-error-boundary";
import { ErrorLayout } from "./ErrorLayout";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

export const DashboardLayout = () => {
  const handleDragMouseDown = async (e: React.MouseEvent) => {
    e.preventDefault();
    try {
      await getCurrentWebviewWindow().startDragging();
    } catch (err) {
      console.error("Failed to start dragging:", err);
    }
  };

  return (
    <ErrorBoundary
      fallbackRender={() => {
        return <ErrorLayout />;
      }}
      resetKeys={["dashboard-error"]}
      onReset={() => {
        console.log("Reset");
      }}
    >
      <div className="relative flex h-screen w-screen overflow-hidden bg-background">
        {/* Draggable region */}
        <div
          className="absolute left-0 right-0 top-0 z-50 h-10 cursor-grab select-none"
          onMouseDown={handleDragMouseDown}
        />

        {/* Sidebar */}
        <Sidebar />
        {/* Main Content */}
        <main className="flex flex-1 flex-col overflow-hidden px-8">
          <Outlet />
        </main>
      </div>
    </ErrorBoundary>
  );
};
