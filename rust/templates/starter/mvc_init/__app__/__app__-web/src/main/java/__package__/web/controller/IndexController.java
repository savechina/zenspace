package __package__.web.controller;

import lombok.extern.slf4j.Slf4j;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

/**
 * HomeController
 *
 * @author weirenyan
 * @date 2023/4/14
 */
@RestController
@Slf4j
public class IndexController {

    @RequestMapping(value = "/")
    public Object test() {
        log.info("application status, success");
        return "status success";
    }
}
