package __package__.bootstrap;


import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.boot.autoconfigure.jdbc.DataSourceAutoConfiguration;
import org.springframework.boot.builder.SpringApplicationBuilder;
import org.springframework.boot.context.properties.EnableConfigurationProperties;
import org.springframework.boot.web.servlet.ServletComponentScan;
import org.springframework.boot.web.servlet.support.SpringBootServletInitializer;
import org.springframework.context.annotation.*;

@SpringBootApplication(exclude = {DataSourceAutoConfiguration.class})
@EnableAspectJAutoProxy(proxyTargetClass = true)
@EnableConfigurationProperties
@Configuration
@ServletComponentScan("__package__.web")
@ComponentScan({"__package__"})
@PropertySource(value = {
         "classpath:important.properties"
})

@ImportResource(value = {
        "classpath:spring-config.xml"})
public class BlueKitSampleApplication extends SpringBootServletInitializer {

    public static void main(String[] args) {
        SpringApplication.run(BlueKitSampleApplication.class, args);
    }

    @Override
    protected SpringApplicationBuilder configure(SpringApplicationBuilder application) {
        return application.sources(BlueKitSampleApplication.class);
    }
}
