import { useTranslation } from "react-i18next";
import { useKanbanStore } from "@/store/useKanbanStore";
import { Button } from "@/components/ui/button";

export function KanbanFeedback() {
  const { t } = useTranslation();
  const { error, pending, loaded, load } = useKanbanStore();
  return (
    <>
      {error && (
        <div
          role="alert"
          className="flex items-center gap-3 rounded-md border border-destructive/40 p-3 text-sm text-destructive"
        >
          <span className="flex-1">
            {t("kanban.saveError")} {error}
          </span>
          <Button
            variant="outline"
            disabled={pending}
            onClick={() => void load()}
          >
            {t("kanban.reload")}
          </Button>
        </div>
      )}
      <span role="status" className="sr-only">
        {pending ? t(loaded ? "kanban.saving" : "kanban.loading") : ""}
      </span>
    </>
  );
}
