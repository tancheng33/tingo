import type { Job } from "./api";
const jobs = new Map<string, Job>();
export const localJobs = (): Job[] => [...jobs.values()];
export function putLocalJob(job: Job): void { jobs.set(job.id!, job); }
export function dismissLocalJob(id: string): boolean { return jobs.delete(id); }
