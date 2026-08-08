package com.burinexample

import java.time.Instant
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertFailsWith

class TaskWorkflowTest {
    private val service = TaskService()

    @Test
    fun newTaskStartsInTODO() {
        val task = service.create("New task")
        assertEquals(TaskStatus.TODO, task.status)
        assertNull(task.completedAt)
    }

    @Test
    fun validPath_TODO_to_IN_PROGRESS_to_DONE() {
        val task = service.create("Long task")
        assertEquals(TaskStatus.TODO, task.status)

        val inProgress = service.transition(task.id, TaskStatus.IN_PROGRESS)
        assertEquals(TaskStatus.IN_PROGRESS, inProgress.status)
        assertNull(inProgress.completedAt)

        val done = service.transition(task.id, TaskStatus.DONE)
        assertEquals(TaskStatus.DONE, done.status)
        assertNotNull(done.completedAt)
    }

    @Test
    fun validPath_TODO_to_CANCELLED() {
        val task = service.create("Short task")
        assertEquals(TaskStatus.TODO, task.status)

        val cancelled = service.transition(task.id, TaskStatus.CANCELLED)
        assertEquals(TaskStatus.CANCELLED, cancelled.status)
        assertNull(cancelled.completedAt)
    }

    @Test
    fun transition_DONE_to_TODO_throws() {
        val task = service.create("Done task")
        service.transition(task.id, TaskStatus.DONE)

        assertFailsWith<IllegalArgumentException> {
            service.transition(task.id, TaskStatus.TODO)
        }
    }

    @Test
    fun parallel_transitions_FROM_IN_PROGRESS_to_DONE() {
        val task1 = service.create("Task A")
        service.transition(task1.id, TaskStatus.IN_PROGRESS)

        val done = service.transition(task1.id, TaskStatus.DONE)
        assertEquals(TaskStatus.DONE, done.status)
        assertNotNull(done.completedAt)
    }

    @Test
    fun parallel_transitions_FROM_IN_PROGRESS_to_CANCELLED() {
        val task1 = service.create("Task B")
        service.transition(task1.id, TaskStatus.IN_PROGRESS)

        val cancelled = service.transition(task1.id, TaskStatus.CANCELLED)
        assertEquals(TaskStatus.CANCELLED, cancelled.status)
        assertNull(cancelled.completedAt)
    }

    @Test
    fun completedAt_is_set_only_on_DONE() {
        // TODO status: no completedAt
        val todoTask = service.create("Todo task")
        assertNull(todoTask.completedAt)

        // IN_PROGRESS status: no completedAt
        val inProgressTask = service.create("In progress task")
        service.transition(inProgressTask.id, TaskStatus.IN_PROGRESS)
        assertNull(inProgressTask.completedAt)

        // DONE status: completedAt is set
        val doneTask = service.create("Done task")
        service.transition(doneTask.id, TaskStatus.DONE)
        assertNotNull(doneTask.completedAt)

        // CANCELLED status: completedAt is null
        val cancelledTask = service.create("Cancelled task")
        service.transition(cancelledTask.id, TaskStatus.CANCELLED)
        assertNull(cancelledTask.completedAt)
    }

    @Test
    fun transition_DONE_to_CANCELLED_throws() {
        val task = service.create("Done task")
        service.transition(task.id, TaskStatus.DONE)

        assertFailsWith<IllegalArgumentException> {
            service.transition(task.id, TaskStatus.CANCELLED)
        }
    }

    @Test
    fun transition_CANCELLED_to_TODO_throws() {
        val task = service.create("Cancelled task")
        service.transition(task.id, TaskStatus.CANCELLED)

        assertFailsWith<IllegalArgumentException> {
            service.transition(task.id, TaskStatus.TODO)
        }
    }

    @Test
    fun transition_CANCELLED_to_IN_PROGRESS_throws() {
        val task = service.create("Cancelled task")
        service.transition(task.id, TaskStatus.CANCELLED)

        assertFailsWith<IllegalArgumentException> {
            service.transition(task.id, TaskStatus.IN_PROGRESS)
        }
    }

    @Test
    fun completedAt_is_null_before_DONE() {
        val task = service.create("Multi-step task")
        assertNull(task.completedAt)

        service.transition(task.id, TaskStatus.IN_PROGRESS)
        assertNull(task.completedAt)
    }

    @Test
    fun completedAt_is_set_after_DONE() {
        val before = Instant.now()
        val task = service.create("Final task")
        service.transition(task.id, TaskStatus.IN_PROGRESS)
        service.transition(task.id, TaskStatus.DONE)
        val after = Instant.now()

        val completedAt = task.completedAt
        assertNotNull(completedAt)
        assertEquals(true, completedAt.isAfter(before) || completedAt == before)
        assertEquals(true, completedAt.isBefore(after) || completedAt == after)
    }
}
