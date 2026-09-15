import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { useAppStore } from "@/store/useAppStore";
import { useKanbanStore } from "@/store/useKanbanStore";
import { useOrganizationNavigation } from "./useOrganizationNavigation";
import { projectKey } from "@/types/kanban";
import type { ClaudeProject } from "@/types";
vi.mock("@/utils/platform", async (original) => ({ ...await original<object>(), isWebUI: () => true }));
vi.mock("@/services/api", () => ({api: vi.fn()}));
const project = {name:"Example",path:"/example",actual_path:"/example",session_count:0,message_count:0,last_modified:"2026-09-10"} as ClaudeProject;
beforeEach(() => {
 window.history.replaceState(null,"","/");
 useAppStore.setState({projects:[project],isLoadingProjects:false,selectedProject:null,selectProject:async(p)=>{useAppStore.setState({selectedProject:p});}});
 useKanbanStore.setState({loaded:true,selectedBoardId:null,data:{revision:0,boards:[{id:"one",name:"One",columns:[{id:"todo",name:"Todo",projectIds:[]}]},{id:"two",name:"Two",columns:[{id:"todo2",name:"Todo",projectIds:[]}]}]}});
});
afterEach(cleanup);
it("restores a direct board URL even without projects and tracks board switches",async()=>{
 useAppStore.setState({projects:[]});
 window.history.replaceState(null,"","/?view=kanban&board=one");
 renderHook(useOrganizationNavigation);
 await waitFor(()=>expect(useAppStore.getState().analytics.currentView).toBe("kanban"));
 expect(useKanbanStore.getState().selectedBoardId).toBe("one");
 act(()=>useKanbanStore.getState().selectBoard("two"));
 expect(new URL(window.location.href).searchParams.get("board")).toBe("two");
 window.history.replaceState(null,"","/?view=kanban&board=one");
 act(()=>window.dispatchEvent(new PopStateEvent("popstate")));
 await waitFor(()=>expect(useKanbanStore.getState().selectedBoardId).toBe("one"));
});
it("restores a project URL into details with its sessions selected for loading",async()=>{
 window.history.replaceState(null,"",`/?view=project&project=${encodeURIComponent(projectKey(project))}`);
 renderHook(useOrganizationNavigation);
 await waitFor(()=>expect(useAppStore.getState().analytics.currentView).toBe("projectDetails"));
 expect(useAppStore.getState().selectedProject?.path).toBe(project.path);
});
