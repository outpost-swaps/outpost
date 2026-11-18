package swap.listener.impl;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.After;
import org.junit.Before;
import org.junit.Test;

public class SwapListenerLockAnnouncementTest {
    @Before
    @After
    public void freshAppRun() {
        SwapListenerImpl.forgetAnnouncedLocks();
    }

    @Test
    public void aLockIsAnnouncedInFullOnceAndThenOnlyAsAResumedLine() {
        assertTrue(SwapListenerImpl.announcesLockInFull("b70ba9e4"));
        assertFalse(SwapListenerImpl.announcesLockInFull("b70ba9e4"));
        assertFalse(SwapListenerImpl.announcesLockInFull("b70ba9e4"));
        assertEquals("Resumed: your Bitcoin is locked in transaction b70ba9e4. The swap carries on from there.",
                SwapListenerImpl.resumedLockLine("b70ba9e4"));
    }

    @Test
    public void anotherSwapsLockIsStillAnnouncedInFull() {
        assertTrue(SwapListenerImpl.announcesLockInFull("aaaa"));
        assertTrue(SwapListenerImpl.announcesLockInFull("bbbb"));
    }

    @Test
    public void aNewAppRunAnnouncesTheLockInFullAgain() {
        assertTrue(SwapListenerImpl.announcesLockInFull("b70ba9e4"));
        SwapListenerImpl.forgetAnnouncedLocks(); // what an app restart is: the memory is gone
        assertTrue(SwapListenerImpl.announcesLockInFull("b70ba9e4"));
    }
}
