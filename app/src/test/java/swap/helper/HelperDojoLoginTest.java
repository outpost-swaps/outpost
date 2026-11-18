package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class HelperDojoLoginTest {
    @Test
    public void aLoginThatReturnsAuthorizationsIsOk() {
        HelperDojoLogin.Result result = HelperDojoLogin.interpret(200, "{\"authorizations\":{\"access_token\":\"x\"}}");

        assertTrue(result.ok());
        assertEquals(HelperDojoLogin.Outcome.OK, result.outcome());
    }

    @Test
    public void aRefusedKeyIsToldApartFromAnUnexpectedAnswer() {
        assertEquals(HelperDojoLogin.Outcome.BAD_KEY, HelperDojoLogin.interpret(401, "{\"error\":\"Invalid API key\"}").outcome());
        assertEquals(HelperDojoLogin.Outcome.BAD_KEY, HelperDojoLogin.interpret(403, "").outcome());
        assertEquals(HelperDojoLogin.Outcome.UNEXPECTED, HelperDojoLogin.interpret(500, "oops").outcome());
        assertFalse(HelperDojoLogin.interpret(200, "<html>not a dojo</html>").ok());
    }

    @Test
    public void theLoginAddressIsTheDojoUrlPlusAuthLogin() {
        assertEquals("http://abc.onion/v2/auth/login", HelperDojoLogin.loginUrl("http://abc.onion/v2"));
        assertEquals("http://abc.onion/test/v2/auth/login", HelperDojoLogin.loginUrl(" http://abc.onion/test/v2/ "));
        assertEquals("https://dojo.example.org/v2/auth/login", HelperDojoLogin.loginUrl("https://dojo.example.org/v2//"));
        assertNull(HelperDojoLogin.loginUrl("ftp://x"));
        assertNull(HelperDojoLogin.loginUrl("abc.onion"));
        assertNull(HelperDojoLogin.loginUrl(null));
    }

    @Test
    public void anUnusableUrlOrAMissingKeyIsRejectedWithoutAnyConnection() {
        // port 1 is never contacted: these fail before a connection is made
        assertEquals(HelperDojoLogin.Outcome.UNEXPECTED, HelperDojoLogin.test("nonsense", "k", 1).outcome());
        assertEquals(HelperDojoLogin.Outcome.UNEXPECTED, HelperDojoLogin.test("http://a.onion/v2", " ", 1).outcome());
    }
}
