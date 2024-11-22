package __package__.web.controller.common;

import io.swagger.annotations.Api;
import lombok.extern.slf4j.Slf4j;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

/**
 * CommonController
 *
 * @author weirenyan
 * @date 2024/5/8
 */
@Slf4j
@Api(value = "CommonController", tags = {"通用接口"})
@RestController
@RequestMapping("/api/common")
public class CommonController {


    @RequestMapping(value = "/status")
    public String status() {
        return "status success";
    }

}
